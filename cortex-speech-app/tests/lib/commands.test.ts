import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import {
  AudioExportFormat,
  ASR_7B_UNAVAILABLE_TAG,
  appGitSha,
  appHealth,
  canRedo,
  canUndo,
  cancelDesktopPlaybackSessionV1,
  commitReviewV1,
  computeDiff,
  clearTracingSpans,
  deleteReviewDraftV1,
  getSettings,
  getActiveVoiceFocusV1,
  getInferenceStats,
  getRecentSpans,
  getReviewDraftV1,
  getVoiceFocusReviewPageV1,
  getTracingStats,
  authoritativeSettingsFromWriteError,
  is7bUnavailableError,
  listAgentImportReports,
  listAgentStageEvents,
  markSegmentUnusableV1,
  normalizeText,
  recordHumanDecision,
  recordReviewFlag,
  redo,
  saveReviewDraftV1,
  takeLastCrash,
  updateSettings,
  undo,
} from '../../src/lib/commands';
import { defaultSettings } from '../../src/lib/stores/settingsStore';
import type { RendererSettingsV1 } from '../../src/lib/generated/ipc';

const invokeMock = vi.mocked(invoke);

describe('generated renderer-safe diagnostics contract', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('uses exact generated commands and preserves the minimized typed DTOs', async () => {
    const tracingStats = {
      total_spans: 4,
      failures: 1,
      total_duration_ms: 12.5,
      avg_duration_ms: 3.125,
    };
    const spans = [
      {
        operation: 'diff.compute',
        start: '2026-08-27T00:00:00Z',
        duration_ms: 4.5,
        success: true,
      },
    ];
    const inference = {
      vad: { calls: 2, failures: 0, p50_ms: 1, p99_ms: 2 },
      asr: { calls: 1, failures: 0, p50_ms: 10, p99_ms: 10 },
      model_load_ms: 25,
    };
    const health = {
      status: 'ok',
      db_size: 1024,
      uptime: 60,
      segment_count: 8,
      memory_mb: 256,
      primary_asr_model: 'LargeV3',
      missing_models: [],
      missing_optional_models: [],
      snapshot_last_success_epoch_secs: 1_787_800_000,
      snapshot_consecutive_failures: 0,
      free_disk_bytes: 5_000_000,
    };
    invokeMock
      .mockResolvedValueOnce(tracingStats)
      .mockResolvedValueOnce(spans)
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(inference)
      .mockResolvedValueOnce(health)
      .mockResolvedValueOnce('the previous session ended unexpectedly (details in the logs folder)')
      .mockResolvedValueOnce('abcdef123456');

    await expect(getTracingStats()).resolves.toEqual(tracingStats);
    await expect(getRecentSpans(50)).resolves.toEqual(spans);
    await expect(clearTracingSpans()).resolves.toBeUndefined();
    await expect(getInferenceStats()).resolves.toEqual(inference);
    await expect(appHealth()).resolves.toEqual(health);
    await expect(takeLastCrash()).resolves.toContain('ended unexpectedly');
    await expect(appGitSha()).resolves.toBe('abcdef123456');

    expect(invokeMock.mock.calls).toEqual([
      ['get_tracing_stats'],
      ['get_recent_spans', { count: 50 }],
      ['clear_tracing_spans'],
      ['get_inference_stats'],
      ['app_health'],
      ['take_last_crash'],
      ['app_git_sha'],
    ]);
  });

  it('preserves a structured diagnostics refusal without exposing a raw string fallback', async () => {
    const refusal = {
      schema: 1,
      code: 'RATE_LIMITED',
      message: 'The diagnostics history is busy. Retry in a moment.',
      retryable: true,
      suggestedAction: 'retry',
      operationId: null,
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(getRecentSpans()).rejects.toBe(refusal);
    expect(invokeMock).toHaveBeenCalledWith('get_recent_spans', { count: null });
  });
});

describe('generated desktop history contract', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('routes all four history commands through their generated names and preserves results', async () => {
    invokeMock
      .mockResolvedValueOnce(true)
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce('Update transcript')
      .mockResolvedValueOnce(null);

    await expect(canUndo()).resolves.toBe(true);
    await expect(canRedo()).resolves.toBe(false);
    await expect(undo()).resolves.toBe('Update transcript');
    await expect(redo()).resolves.toBeNull();
    expect(invokeMock.mock.calls).toEqual([['can_undo'], ['can_redo'], ['undo'], ['redo']]);
  });

  it('preserves a structured typed refusal without converting it to a raw string', async () => {
    const refusal = {
      schema: 1,
      code: 'UNDO_FAILED',
      message: 'The last change could not be undone.',
      retryable: false,
      suggestedAction: 'openHealth',
      operationId: null,
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(undo()).rejects.toBe(refusal);
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith('undo');
  });
});

describe('generated transcript utility contract', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('preserves exact normalization and diff command names, arguments, and results', async () => {
    const diff = {
      raw: 'a c',
      annotated: 'a b c',
      changes: [
        { op: 'Equal' as const, value: 'a' },
        { op: 'Insert' as const, value: 'b' },
        { op: 'Equal' as const, value: 'c' },
      ],
      stats: {
        added_words: 1,
        removed_words: 0,
        changed_words: 0,
        unchanged_words: 2,
        similarity: 200 / 3,
      },
    };
    invokeMock.mockResolvedValueOnce('normalized').mockResolvedValueOnce(diff);

    await expect(normalizeText('raw')).resolves.toBe('normalized');
    await expect(computeDiff('a c', 'a b c')).resolves.toEqual(diff);
    expect(invokeMock.mock.calls).toEqual([
      ['normalize_text', { text: 'raw' }],
      ['compute_diff', { raw: 'a c', annotated: 'a b c' }],
    ]);
  });

  it('preserves a typed memory refusal without converting it to prose', async () => {
    const refusal = {
      schema: 1,
      code: 'DIFF_TOO_COMPLEX',
      message: 'The transcript comparison would require too much memory.',
      retryable: false,
      suggestedAction: null,
      operationId: null,
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(computeDiff('raw', 'annotated')).rejects.toBe(refusal);
  });
});

function rendererSettings(overrides: Partial<RendererSettingsV1> = {}): RendererSettingsV1 {
  return {
    asr_model_size: 'WSL7B',
    use_finetuned_asr: false,
    vad_threshold: 0.5,
    min_segment_duration_ms: 3000,
    max_segment_duration_ms: 15000,
    num_asr_threads: 4,
    enable_gpu: true,
    language: 'ckb',
    export_format: 'Json',
    auto_normalize: true,
    verbalize_numbers: true,
    auto_align: false,
    assign_speaker_from_filename: true,
    enable_diarization: true,
    enable_denoising: false,
    autoplay_segments: false,
    max_speakers: 8,
    max_wer_threshold: 0.35,
    max_cer_threshold: 0.2,
    enforce_quality_gates: false,
    theme: 'Dark',
    llm_mode: 'None',
    llm_endpoint: 'http://127.0.0.1:11434/v1/chat/completions',
    llm_api_key_configured: false,
    cloud_llm_opt_in: false,
    llm_system_prompt: defaultSettings.llmSystemPrompt,
    llm_model: 'heretic-final:latest',
    external_asr_script_path: '',
    hf_train_ratio: 0.8,
    hf_val_ratio: 0.1,
    hf_test_ratio: 0.1,
    hf_split_seed: 42,
    hf_speaker_disjoint: true,
    hf_license: 'mit',
    jury_cloud_opt_in: false,
    jury_model: 'gemini-2.5-pro',
    jury_provider: 'gemini',
    source_reference_models: ['gemini-2.5-pro'],
    jury_self_consistency_n: 3,
    jury_autonomy_level: 'propose',
    jury_t1_threshold: 0.75,
    ...overrides,
  };
}

describe('7B-champion-unavailable detection (never silently downgrade)', () => {
  it('matches the sentinel whether the error is a bare string or an Error object', () => {
    // Tauri rejects invoke() with the backend error STRING; some paths wrap it in an Error.
    expect(is7bUnavailableError(`${ASR_7B_UNAVAILABLE_TAG}: server not responding`)).toBe(true);
    expect(is7bUnavailableError(new Error(`${ASR_7B_UNAVAILABLE_TAG}: timed out`))).toBe(true);
  });

  it('does NOT match ordinary transcription errors (those show the normal error, not the choice)', () => {
    expect(is7bUnavailableError('Empty audio file')).toBe(false);
    expect(is7bUnavailableError(new Error('ONNX inference failed'))).toBe(false);
    expect(is7bUnavailableError(null)).toBe(false);
    expect(is7bUnavailableError(undefined)).toBe(false);
  });
});

describe('revision-guarded generated settings contract', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('loads settings and their opaque revision in one generated snapshot', async () => {
    invokeMock.mockResolvedValueOnce({ settingsRevision: 101, settings: rendererSettings() });

    await expect(getSettings()).resolves.toMatchObject(defaultSettings);
    expect(invokeMock).toHaveBeenCalledWith('get_settings_v1');
  });

  it('writes only changed non-secret preference fields against the loaded revision', async () => {
    const initial = rendererSettings();
    const committed = rendererSettings({ autoplay_segments: true });
    invokeMock
      .mockResolvedValueOnce({ settingsRevision: 201, settings: initial })
      .mockResolvedValueOnce({
        settingsRevision: 202,
        settings: committed,
        alreadyApplied: false,
      });

    const loaded = await getSettings();
    await updateSettings({
      ...loaded,
      autoplaySegments: true,
      llmApiKey: 'must-use-the-secret-command',
    });

    expect(invokeMock.mock.calls).toEqual([
      ['get_settings_v1'],
      [
        'patch_settings_v1',
        {
          patch: {
            expectedSettingsRevision: 201,
            changedFields: { autoplay_segments: true },
          },
        },
      ],
    ]);
  });

  it('keeps consent out of the generic patch and grants only after preferences persist', async () => {
    const initial = rendererSettings();
    const preferences = rendererSettings({ autoplay_segments: true });
    const granted = rendererSettings({
      autoplay_segments: true,
      cloud_llm_opt_in: true,
    });
    invokeMock
      .mockResolvedValueOnce({ settingsRevision: 301, settings: initial })
      .mockResolvedValueOnce({
        settingsRevision: 302,
        settings: preferences,
        alreadyApplied: false,
      })
      .mockResolvedValueOnce({
        settingsRevision: 303,
        settings: granted,
        alreadyApplied: false,
      });

    const loaded = await getSettings();
    await updateSettings({ ...loaded, autoplaySegments: true, cloudLlmOptIn: true });

    expect(invokeMock.mock.calls.slice(1)).toEqual([
      [
        'patch_settings_v1',
        {
          patch: {
            expectedSettingsRevision: 301,
            changedFields: { autoplay_segments: true },
          },
        },
      ],
      [
        'set_cloud_consent_v1',
        {
          request: { expectedSettingsRevision: 302, consent: 'llm', granted: true },
        },
      ],
    ]);
  });

  it('replays a transport-uncertain patch once with the byte-identical CAS payload', async () => {
    const initial = rendererSettings();
    const committed = rendererSettings({ autoplay_segments: true });
    invokeMock
      .mockResolvedValueOnce({ settingsRevision: 401, settings: initial })
      .mockRejectedValueOnce(new Error('response lost'))
      .mockResolvedValueOnce({
        settingsRevision: 402,
        settings: committed,
        alreadyApplied: true,
      });

    const loaded = await getSettings();
    await updateSettings({ ...loaded, autoplaySegments: true });

    expect(invokeMock).toHaveBeenCalledTimes(3);
    expect(invokeMock.mock.calls[2]).toEqual(invokeMock.mock.calls[1]);
  });

  it('never retries a structured stale refusal and attaches fresh server truth for rollback', async () => {
    const initial = rendererSettings();
    const authoritative = rendererSettings({ jury_autonomy_level: 'act_confirm' });
    const stale = {
      schema: 1,
      code: 'STALE_SETTINGS_REVISION',
      message: 'reload',
      retryable: false,
      suggestedAction: null,
      operationId: null,
      details: { expectedSettingsRevision: 501, currentSettingsRevision: 502 },
    };
    invokeMock
      .mockResolvedValueOnce({ settingsRevision: 501, settings: initial })
      .mockRejectedValueOnce(stale)
      .mockResolvedValueOnce({ settingsRevision: 502, settings: authoritative });

    const loaded = await getSettings();
    let failure: unknown;
    try {
      await updateSettings({ ...loaded, autoplaySegments: true });
    } catch (error) {
      failure = error;
    }

    expect(invokeMock.mock.calls.map(([command]) => command)).toEqual([
      'get_settings_v1',
      'patch_settings_v1',
      'get_settings_v1',
    ]);
    expect(authoritativeSettingsFromWriteError(failure)).toMatchObject({
      juryAutonomyLevel: 'act_confirm',
      autoplaySegments: false,
    });
  });
});

describe('commands audio export contract', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('only advertises formats the backend can actually encode', () => {
    expect(AudioExportFormat).toEqual({ Wav: 'Wav' });
    expect(Object.values(AudioExportFormat)).not.toContain('Flac');
  });

  it('lists agent import reports through the registered backend command', async () => {
    invokeMock.mockResolvedValueOnce([]);

    await expect(listAgentImportReports(7)).resolves.toEqual([]);

    expect(invokeMock).toHaveBeenCalledWith('list_agent_import_reports', { limit: 7 });
  });

  it('lists persisted agent stage events through the registered backend command', async () => {
    invokeMock.mockResolvedValueOnce([]);

    await expect(listAgentStageEvents('run-1', 9)).resolves.toEqual([]);

    expect(invokeMock).toHaveBeenCalledWith('list_agent_stage_events', {
      runId: 'run-1',
      limit: 9,
    });
  });
});

describe('opaque voice-focus review contract', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('discovers only the renderer-safe focus identity and cardinality', async () => {
    const active = { focusId: `vf1_${'a'.repeat(64)}`, segmentCount: 2 };
    invokeMock.mockResolvedValueOnce(active);

    await expect(getActiveVoiceFocusV1()).resolves.toEqual(active);
    expect(invokeMock).toHaveBeenCalledWith('get_active_voice_focus_v1');
  });

  it('binds review paging to the exact discovered identity without a legacy invoke path', async () => {
    const focusId = `vf1_${'b'.repeat(64)}`;
    const page = {
      items: [],
      total: 0,
      nextCursor: null,
      scopeLabel: 'voiceFocus',
      focusNarrowed: true,
    };
    invokeMock.mockResolvedValueOnce(page);

    await expect(getVoiceFocusReviewPageV1(focusId, 'cursor_1', 25)).resolves.toEqual(page);
    expect(invokeMock).toHaveBeenCalledWith('get_review_page_v1', {
      scope: { kind: 'voiceFocus', focusId },
      limit: 25,
      cursor: 'cursor_1',
    });
  });

  it('preserves the structured stale-policy refusal without retry or string fallback', async () => {
    const refusal = {
      schema: 1,
      code: 'STALE_VOICE_FOCUS',
      message: 'reload',
      retryable: false,
      suggestedAction: 'reloadClip',
      operationId: null,
      details: {},
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(getVoiceFocusReviewPageV1(`vf1_${'c'.repeat(64)}`)).rejects.toBe(refusal);
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });
});

describe('desktop playback cancellation contract', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('sends the exact receipt and client-attempt identities and preserves the idempotent result', async () => {
    invokeMock.mockResolvedValueOnce(false);

    await expect(
      cancelDesktopPlaybackSessionV1(
        '11111111-1111-4111-8111-111111111111',
        '22222222-2222-4222-8222-222222222222',
      ),
    ).resolves.toBe(false);
    expect(invokeMock).toHaveBeenCalledWith('cancel_desktop_playback_session_v1', {
      playbackReceiptId: '11111111-1111-4111-8111-111111111111',
      clientAttemptId: '22222222-2222-4222-8222-222222222222',
    });
  });
});

describe('desktop review decision idempotency', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('fails closed locally instead of invoking the retired ambient-receipt writer', async () => {
    await expect(
      recordHumanDecision('segment-1', 'edit', 'دەقی ڕاست', 1_777_000),
    ).rejects.toMatchObject({
      schema: 1,
      code: 'TYPED_REVIEW_REQUIRED',
      retryable: false,
    });
    expect(invokeMock).not.toHaveBeenCalled();
  });
});

describe('typed desktop review decision idempotency', () => {
  const request = {
    operationId: '44444444-4444-4444-8444-444444444444',
    segmentId: 'segment-typed',
    baseRevision: 7,
    decision: 'edit' as const,
    transcript: 'دەقی ڕاست',
    reasonCode: null,
    playbackReceiptId: '77777777-7777-4777-8777-777777777777',
  };

  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('replays only a transport-level uncertainty with the exact generated payload', async () => {
    const commit = {
      segmentId: request.segmentId,
      committedRevision: 8,
      authoritativeTranscript: request.transcript,
      decisionId: 'effect:41',
    };
    invokeMock
      .mockRejectedValueOnce(new Error('transport response lost'))
      .mockResolvedValueOnce(commit);

    await expect(commitReviewV1(request)).resolves.toEqual(commit);

    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(invokeMock.mock.calls[0]).toEqual(['commit_review_v1', { request }]);
    expect(invokeMock.mock.calls[1]).toEqual(invokeMock.mock.calls[0]);
  });

  it('never retries a structured backend refusal', async () => {
    const refusal = {
      schema: 1,
      code: 'STALE_REVISION',
      message: 'stale',
      retryable: false,
      suggestedAction: 'reloadClip',
      operationId: request.operationId,
      details: { expectedRevision: 7, currentRevision: 8 },
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(commitReviewV1(request)).rejects.toBe(refusal);
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });
});

describe('revision-bound desktop review drafts', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('uses only generated typed commands for load, save, and guarded delete', async () => {
    const draft = {
      segmentId: 'segment-draft',
      baseRevision: 9,
      text: 'دەقی ناتەواو',
      updatedAt: '2026-08-25T12:00:00.000Z',
    };
    invokeMock
      .mockResolvedValueOnce(draft)
      .mockResolvedValueOnce(draft)
      .mockResolvedValueOnce(true);

    await expect(getReviewDraftV1(draft.segmentId)).resolves.toEqual(draft);
    await expect(
      saveReviewDraftV1(draft.segmentId, draft.baseRevision, draft.text),
    ).resolves.toEqual(draft);
    await expect(deleteReviewDraftV1(draft.segmentId, draft.baseRevision)).resolves.toBe(true);

    expect(invokeMock.mock.calls).toEqual([
      ['get_review_draft_v1', { segmentId: draft.segmentId }],
      [
        'save_review_draft_v1',
        { segmentId: draft.segmentId, baseRevision: draft.baseRevision, text: draft.text },
      ],
      ['delete_review_draft_v1', { segmentId: draft.segmentId, baseRevision: draft.baseRevision }],
    ]);
  });

  it('does not retry or stringify a stale-revision draft refusal', async () => {
    const refusal = {
      schema: 1,
      code: 'STALE_DRAFT_REVISION',
      message: 'reload',
      retryable: false,
      suggestedAction: 'reloadClip',
      operationId: null,
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(saveReviewDraftV1('segment-draft', 9, 'text')).rejects.toBe(refusal);
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });
});

describe('typed technical-unusable idempotency', () => {
  const request = {
    operationId: '55555555-5555-4555-8555-555555555555',
    segmentId: 'segment-unusable',
    baseRevision: 12,
    reason: 'corruptContainer' as const,
  };

  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('replays one transport uncertainty with the exact same closed reason and operation id', async () => {
    const committed = {
      segmentId: request.segmentId,
      committedRevision: 13,
      reason: request.reason,
      effectId: 'flag-effect:72',
    };
    invokeMock
      .mockRejectedValueOnce(new Error('transport response lost'))
      .mockResolvedValueOnce(committed);

    await expect(markSegmentUnusableV1(request)).resolves.toEqual(committed);

    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(invokeMock.mock.calls[0]).toEqual(['mark_segment_unusable_v1', { request }]);
    expect(invokeMock.mock.calls[1]).toEqual(invokeMock.mock.calls[0]);
  });

  it('never retries a structured revision refusal', async () => {
    const refusal = {
      schema: 1,
      code: 'STALE_REVISION',
      message: 'reload this clip',
      retryable: false,
      suggestedAction: 'reloadClip',
      operationId: request.operationId,
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(markSegmentUnusableV1(request)).rejects.toBe(refusal);
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });
});

describe('desktop review flag idempotency', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('replays one uncertain invoke with the exact same flag operation identity and payload', async () => {
    const commit = {
      effectEventId: 52,
      segmentId: 'segment-2',
      priorRevision: 7,
      flagRevision: 8,
      segment: { id: 'segment-2' },
    };
    invokeMock
      .mockRejectedValueOnce(new Error('transport response lost'))
      .mockResolvedValueOnce(commit);

    await expect(recordReviewFlag('segment-2', 'needs a second listen')).resolves.toBe(commit);

    expect(invokeMock).toHaveBeenCalledTimes(2);
    const first = invokeMock.mock.calls[0];
    const second = invokeMock.mock.calls[1];
    expect(first[0]).toBe('record_review_flag');
    expect(second[0]).toBe('record_review_flag');
    expect(second[1]).toEqual(first[1]);
    expect(first[1]).toMatchObject({
      segmentId: 'segment-2',
      rationale: 'needs a second listen',
      operationId: expect.stringMatching(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
      ),
    });
  });
});
