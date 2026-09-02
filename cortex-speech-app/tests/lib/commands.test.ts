import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import {
  AudioExportFormat,
  ASR_7B_UNAVAILABLE_TAG,
  alignSegment,
  appGitSha,
  appHealth,
  cancelDesktopPlaybackSessionV1,
  commitReviewV1,
  computeSignalAnomalyScores,
  computeDiff,
  clearTracingSpans,
  deleteReviewDraftV1,
  createGoldFromFile,
  exportAudio,
  exportDataset,
  exportFinetunePack,
  exportGoldEvalSet,
  exportHuggingfaceDataset,
  exportTranscript,
  buildScorecard,
  getAudioHealth,
  getSettings,
  getActiveVoiceFocusV1,
  getActiveLearningQueue,
  getEscalationQueue,
  getEscalationRateTrend,
  getHistoryStatusV1,
  getSegmentConsensus,
  getInferenceStats,
  getIntelligenceReport,
  getRecentSpans,
  getReviewDraftV1,
  getSegment,
  getSegmentIdsForView,
  getSegmentsPage,
  getSignalAnomalySegments,
  getVoiceFocusReviewPageV1,
  getTracingStats,
  getWaveform,
  authoritativeSettingsFromWriteError,
  is7bUnavailableError,
  isCommandErrorV1,
  importAudioFile,
  importDirectory,
  importVerifiedSegmentsAsGold,
  listAgentImportReports,
  listAgentStageEvents,
  listEvalRuns,
  markSegmentUnusableV1,
  mergeDatasetJson,
  normalizeText,
  openAudioFile,
  recordHumanDecision,
  recordReviewFlag,
  relinkAudio,
  redo,
  runGoldEvalAsr,
  runJuryPipeline,
  runT2ForSegment,
  runWslRefinement,
  saveReviewDraftV1,
  takeLastCrash,
  transcribeSegment,
  updateSettings,
  undo,
  validateDataset,
} from '../../src/lib/commands';
import { defaultSettings } from '../../src/lib/stores/settingsStore';
import type { RendererSettingsV1 } from '../../src/lib/generated/ipc';

const invokeMock = vi.mocked(invoke);

describe('typed command-error classification', () => {
  it('recognizes a complete V1 refusal and an optional exact code', () => {
    const refusal = {
      schema: 1,
      code: 'STALE_REVISION',
      message: 'The segment changed.',
      retryable: false,
    };

    expect(isCommandErrorV1(refusal)).toBe(true);
    expect(isCommandErrorV1(refusal, 'STALE_REVISION')).toBe(true);
    expect(isCommandErrorV1(refusal, 'SEGMENT_NOT_FOUND')).toBe(false);
  });

  it('is total for hostile IPC values and reads each required field once', () => {
    const throwing = new Proxy(
      {},
      {
        get() {
          throw new Error('hostile getter');
        },
      },
    );
    expect(() => isCommandErrorV1(throwing, 'STALE_REVISION')).not.toThrow();
    expect(isCommandErrorV1(throwing, 'STALE_REVISION')).toBe(false);

    const reads = new Map<PropertyKey, number>();
    const stateful = new Proxy(
      {},
      {
        get(_target, property) {
          reads.set(property, (reads.get(property) ?? 0) + 1);
          if (property === 'schema') return 1;
          if (property === 'code') return 'STALE_REVISION';
          if (property === 'message') return 'The segment changed.';
          if (property === 'retryable') return false;
          return undefined;
        },
      },
    );

    expect(isCommandErrorV1(stateful, 'STALE_REVISION')).toBe(true);
    expect(Object.fromEntries(reads)).toEqual({ schema: 1, code: 1, message: 1, retryable: 1 });
  });
});

describe('generated owner critical-loop contract', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('uses the nine generated commands with exact arguments and preserves their wire results', async () => {
    const runId = '85c9ce7e-9a91-4bd4-b1f0-e3f278ad5f7a';
    const transcription = {
      text: 'دەقی پاک',
      rawTranscript: 'دەقی خام',
      confidence: 0.94,
      confidenceSource: 'real_posterior',
      modelVersionId: 'champion-sha',
      cloudCall: false,
      segmentId: 'segment-1',
    };
    const timestamps = [{ word: 'دەقی', start: 0, end: 0.4, confidence: 0.9 }];
    const consensus = {
      draft: 'دەقی پاک',
      words: [
        {
          text: 'دەقی',
          agreement: 1,
          modelsAgreeing: 1,
          totalModels: 1,
          alternatives: [],
        },
      ],
      modelCount: 1,
      minAgreement: 1,
      meanAgreement: 1,
      models: ['champion-sha'],
    };
    invokeMock
      .mockResolvedValueOnce('D:/audio/source.wav')
      .mockResolvedValueOnce({ status: 'started', runId })
      .mockResolvedValueOnce({ status: 'started', source: 'file', runId })
      .mockResolvedValueOnce(transcription)
      .mockResolvedValueOnce(timestamps)
      .mockResolvedValueOnce(consensus)
      .mockResolvedValueOnce([0.1, 0.4, 0.2])
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(null);

    await expect(openAudioFile()).resolves.toBe('D:/audio/source.wav');
    await expect(importDirectory(runId)).resolves.toEqual({ status: 'started', runId });
    await expect(importAudioFile('D:/audio/source.wav', runId)).resolves.toEqual({
      status: 'started',
      source: 'file',
      runId,
    });
    await expect(
      transcribeSegment(
        'D:/audio/source.wav',
        '{"source_start_ms":0,"source_end_ms":1000}',
        'segment-1',
      ),
    ).resolves.toEqual(transcription);
    await expect(
      alignSegment(
        'D:/audio/source.wav',
        'دەقی پاک',
        '{"source_start_ms":0,"source_end_ms":1000}',
        'segment-1',
      ),
    ).resolves.toEqual(timestamps);
    await expect(getSegmentConsensus('segment-1')).resolves.toEqual(consensus);
    await expect(getWaveform('D:/audio/source.wav', 512, null)).resolves.toEqual([0.1, 0.4, 0.2]);
    await expect(exportDataset('D:/proof/library.jsonl', 'jsonl')).resolves.toBeUndefined();
    await expect(exportTranscript('D:/proof/library.txt', 'txt')).resolves.toBeUndefined();

    expect(invokeMock.mock.calls).toEqual([
      ['open_audio_file'],
      ['import_directory', { runId }],
      ['import_audio_file', { path: 'D:/audio/source.wav', runId }],
      [
        'transcribe_segment',
        {
          segmentId: 'segment-1',
          audioPath: 'D:/audio/source.wav',
          alignmentJson: '{"source_start_ms":0,"source_end_ms":1000}',
        },
      ],
      [
        'align_segment',
        {
          audioPath: 'D:/audio/source.wav',
          text: 'دەقی پاک',
          alignmentJson: '{"source_start_ms":0,"source_end_ms":1000}',
          segmentId: 'segment-1',
        },
      ],
      ['get_segment_consensus', { segmentId: 'segment-1' }],
      ['get_waveform', { path: 'D:/audio/source.wav', numPoints: 512, alignmentJson: null }],
      ['export_dataset', { path: 'D:/proof/library.jsonl', format: 'jsonl' }],
      ['export_transcript', { path: 'D:/proof/library.txt', format: 'txt' }],
    ]);
  });

  it('preserves a structured champion hard-stop without converting it to a string', async () => {
    const refusal = {
      schema: 1,
      code: ASR_7B_UNAVAILABLE_TAG,
      message: `${ASR_7B_UNAVAILABLE_TAG}: The pinned champion is unavailable.`,
      retryable: true,
      suggestedAction: 'openModels' as const,
      operationId: null,
      details: {},
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(transcribeSegment('D:/audio/source.wav', null, 'segment-1')).rejects.toBe(refusal);
    expect(is7bUnavailableError(refusal)).toBe(true);
  });
});

describe('generated owner analysis contract', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('uses the eleven generated commands with exact arguments and preserves their wire results', async () => {
    const evalRun = {
      id: 'eval-1',
      modelId: 'omniasr-7b@champion-sha',
      runAt: '2026-08-28T10:00:00.000Z',
      numSegs: 1,
      wer: 0.1,
      cer: 0.05,
      metaJson: null,
    };
    const evalResult = {
      run: evalRun,
      segments: [
        {
          goldId: 'gold-1',
          audioPath: 'D:/private/gold-1.wav',
          reference: 'دەقی ڕاست',
          hypothesis: 'دەقی ڕاست',
          wer: 0,
          cer: 0,
        },
      ],
    };
    const intelligence = {
      loop0Shadow: {
        totalObservations: 10,
        wouldFire: 2,
        firedButHumanAcceptedOriginal: 0,
        firedAndHumanEdited: 2,
        firedAndHumanRejected: 0,
      },
      autoAcceptPrecision: {
        t0Accepts: 0,
        t1Escalations: 10,
        t0HumanConfirmed: 0,
        t0HumanContradicted: 0,
      },
      conformalCalibration: {
        targetErrorCer: 0.02,
        perBucketDelta: 0.002,
        minNeededAtZeroCer: 1497,
        buckets: [{ bucket: 'unknown', verifiedWithReference: 10, minNeededAtZeroCer: 1497 }],
      },
    };
    const trend = [{ date: '2026-08-28', escalationRate: 1, total: 10, escalated: 10 }];
    const jury = {
      mode: 'not_required' as const,
      totalInput: 1,
      t0AutoAccepted: 0,
      t0Escalated: 0,
      referenceCommitted: 0,
      referenceGuarded: 0,
      hypothesisGuarded: 0,
      t1Committed: 0,
      t2Committed: 0,
      humanInbox: 0,
      reason: 'Human review is authoritative.',
    };
    const t2 = { verdict: null, mustEscalate: true, error: 'T2_JUDGE_UNAVAILABLE' };
    const scorecard = {
      scorecard: {
        system: {
          modelId: evalRun.modelId,
          numSegments: 1,
          scoredSegments: 1,
          microWer: 0,
          microCer: 0,
          macroWer: 0,
          substitutions: 0,
          deletions: 0,
          insertions: 0,
          werCi: { lower: 0, upper: 0 },
          cerCi: { lower: 0, upper: 0 },
        },
        bootstrapResamples: 1000,
        confidence: 0.95,
        seed: 42,
      },
      markdown: '# Scorecard',
    };
    const segment = { id: 'segment-1' };

    invokeMock
      .mockResolvedValueOnce(intelligence)
      .mockResolvedValueOnce({ status: 'started' })
      .mockResolvedValueOnce(7)
      .mockResolvedValueOnce([segment])
      .mockResolvedValueOnce(evalResult)
      .mockResolvedValueOnce(scorecard)
      .mockResolvedValueOnce([evalRun])
      .mockResolvedValueOnce([segment])
      .mockResolvedValueOnce(trend)
      .mockResolvedValueOnce(jury)
      .mockResolvedValueOnce(t2);

    await expect(getIntelligenceReport()).resolves.toEqual(intelligence);
    await expect(
      runWslRefinement({ limit_files: 3, limit_segments: 12, dry_run: true, test_one: false }),
    ).resolves.toEqual({ status: 'started' });
    await expect(computeSignalAnomalyScores()).resolves.toBe(7);
    await expect(getActiveLearningQueue(0.02, 0.95, 25)).resolves.toEqual([segment]);
    await expect(runGoldEvalAsr()).resolves.toEqual(evalResult);
    await expect(buildScorecard(evalResult, null)).resolves.toEqual(scorecard);
    await expect(listEvalRuns()).resolves.toEqual([evalRun]);
    await expect(getEscalationQueue(40)).resolves.toEqual([segment]);
    await expect(getEscalationRateTrend()).resolves.toEqual(trend);
    await expect(runJuryPipeline(['segment-1'])).resolves.toEqual(jury);
    await expect(runT2ForSegment('segment-1', 'renderer-secret')).resolves.toEqual(t2);

    expect(invokeMock.mock.calls).toEqual([
      ['get_intelligence_report'],
      ['run_wsl_refinement', { limitFiles: 3, limitSegments: 12, dryRun: true, testOne: false }],
      ['compute_signal_anomaly_scores'],
      ['get_active_learning_queue', { targetError: 0.02, confidenceLevel: 0.95, limit: 25 }],
      ['run_gold_eval_asr'],
      ['build_scorecard', { system: evalResult, baseline: null }],
      ['list_eval_runs'],
      ['get_escalation_queue', { limit: 40 }],
      ['get_escalation_rate_trend'],
      ['run_jury_pipeline', { segmentIds: ['segment-1'] }],
      ['run_t2_for_segment', { segmentId: 'segment-1', apiKey: 'renderer-secret' }],
    ]);
  });

  it('preserves a structured owner-analysis refusal without retry or string coercion', async () => {
    const refusal = {
      schema: 1,
      code: 'ANALYSIS_UNAVAILABLE',
      message: 'The analysis could not be completed. Try again.',
      retryable: true,
      suggestedAction: 'retry' as const,
      operationId: null,
      details: {},
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(getIntelligenceReport()).rejects.toBe(refusal);
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });
});

describe('generated owner data and export contract', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('uses the ten generated commands with exact arguments and complete wire results', async () => {
    const audioHealth = {
      totalFiles: 3,
      missingFiles: 1,
      missingPaths: ['D:/owner/missing.wav'],
    };
    const relink = { relinked: 1, stillMissing: 0 };
    const validation = {
      totalSegments: 2,
      totalAudioFiles: 1,
      passed: 1,
      warnings: [
        {
          severity: 'Warning',
          category: 'AlignmentHeuristic',
          segmentId: 'segment-2',
          field: 'alignment_json',
          message: 'Alignment requires review.',
          details: null,
        },
      ],
      errors: [],
      summary: '1 passed, 1 warning',
    } as const;
    const audioExport = {
      total: 2,
      succeeded: 1,
      failed: 1,
      output_dir: 'D:/proof/audio',
      files: ['segment-1.wav'],
      errors: ['AUDIO_EXPORT_ITEM_FAILED'],
    };
    const goldExport = {
      manifestPath: 'D:/proof/gold/manifest.jsonl',
      totalGold: 2,
      exported: 2,
      skipped: 0,
    };
    const finetuneExport = {
      manifestPath: 'D:/proof/train/manifest.jsonl',
      manifestSha256: 'abc123',
      totalVerified: 4,
      excludedUnexportable: 1,
      excludedNotTrainingReady: 1,
      emitted: 2,
      skipped: 0,
      emittedWithoutHumanDecision: 0,
      snapshotId: 'abc123',
      newlySealed: true,
    };
    invokeMock
      .mockResolvedValueOnce(audioHealth)
      .mockResolvedValueOnce(relink)
      .mockResolvedValueOnce(validation)
      .mockResolvedValueOnce(audioExport)
      .mockResolvedValueOnce({ created: 2, updated: 1 })
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(3)
      .mockResolvedValueOnce(4)
      .mockResolvedValueOnce(goldExport)
      .mockResolvedValueOnce(finetuneExport);

    await expect(getAudioHealth()).resolves.toEqual(audioHealth);
    await expect(relinkAudio('D:/owner/audio')).resolves.toEqual(relink);
    await expect(validateDataset()).resolves.toEqual(validation);
    await expect(
      exportAudio(['segment-1', 'segment-2'], {
        output_dir: 'D:/proof/audio',
        format: AudioExportFormat.Wav,
        sample_rate: 16_000,
        include_metadata: true,
      }),
    ).resolves.toEqual(audioExport);
    await expect(mergeDatasetJson('{"segments":[]}')).resolves.toEqual({ created: 2, updated: 1 });
    await expect(exportHuggingfaceDataset('D:/proof/hf')).resolves.toBeUndefined();
    await expect(createGoldFromFile('D:/owner/source.wav')).resolves.toBe(3);
    await expect(importVerifiedSegmentsAsGold()).resolves.toBe(4);
    await expect(exportGoldEvalSet('D:/proof/gold')).resolves.toEqual(goldExport);
    await expect(exportFinetunePack('D:/proof/train')).resolves.toEqual(finetuneExport);

    expect(invokeMock.mock.calls).toEqual([
      ['get_audio_health'],
      ['relink_audio', { searchDir: 'D:/owner/audio' }],
      ['validate_dataset_cmd'],
      [
        'export_audio',
        {
          segmentIds: ['segment-1', 'segment-2'],
          options: {
            output_dir: 'D:/proof/audio',
            format: 'Wav',
            sample_rate: 16_000,
            include_metadata: true,
          },
        },
      ],
      ['merge_dataset_json', { jsonContent: '{"segments":[]}' }],
      ['export_huggingface_dataset', { path: 'D:/proof/hf' }],
      ['create_gold_from_file', { audioPath: 'D:/owner/source.wav' }],
      ['import_verified_segments_as_gold'],
      ['export_gold_eval_set', { outDir: 'D:/proof/gold' }],
      ['export_finetune_pack', { outDir: 'D:/proof/train' }],
    ]);
  });

  it('preserves structured private-safe refusals without string coercion', async () => {
    const refusal = {
      schema: 1,
      code: 'DATASET_VALIDATION_FAILED',
      message: 'Dataset validation could not produce a complete report.',
      retryable: false,
      suggestedAction: 'openHealth' as const,
      operationId: null,
      details: {},
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(validateDataset()).rejects.toBe(refusal);
  });
});

describe('generated library read contract', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('uses exact generated commands and preserves page scope and revision evidence', async () => {
    const segment = {
      id: 'segment-1',
      audioPath: 'fixture.wav',
      rawTranscript: 'دەق',
      normalizedTranscript: null,
      annotatedTranscript: null,
      alignmentJson: null,
      durationMs: 1000,
      speakerId: null,
      verified: false,
    };
    const page = {
      items: [segment],
      total: 1,
      nextCursor: 'cursor_2',
      revisions: { 'segment-1': 7 },
      focusNarrowed: true,
    };
    invokeMock
      .mockResolvedValueOnce(segment)
      .mockResolvedValueOnce(page)
      .mockResolvedValueOnce(['segment-1'])
      .mockResolvedValueOnce([segment]);

    await expect(getSegment('segment-1')).resolves.toEqual(segment);
    await expect(
      getSegmentsPage({
        verified: false,
        query: 'دەق',
        sort: 'oldest',
        limit: 25,
        cursor: 'cursor_1',
        focused: true,
      }),
    ).resolves.toEqual(page);
    await expect(
      getSegmentIdsForView({ verified: false, query: 'دەق', transcriptState: 'real' }),
    ).resolves.toEqual(['segment-1']);
    await expect(getSignalAnomalySegments(25)).resolves.toEqual([segment]);

    expect(invokeMock.mock.calls).toEqual([
      ['get_segment', { segmentId: 'segment-1' }],
      [
        'get_segments_page',
        {
          verified: false,
          query: 'دەق',
          sort: 'oldest',
          limit: 25,
          cursor: 'cursor_1',
          focused: true,
        },
      ],
      ['get_segment_ids_for_view', { verified: false, query: 'دەق', transcriptState: 'real' }],
      ['get_signal_anomaly_segments', { limit: 25 }],
    ]);
  });

  it('preserves a structured library refusal without manufacturing an empty result', async () => {
    const refusal = {
      schema: 1,
      code: 'LIBRARY_READ_FAILED',
      message: 'The library could not be read. Open Health for recovery options.',
      retryable: false,
      suggestedAction: 'openHealth',
      operationId: null,
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(getSegmentsPage()).rejects.toBe(refusal);
  });

  it('rejects a malformed success payload instead of presenting an empty library', async () => {
    invokeMock.mockResolvedValueOnce({ items: [], total: '1', nextCursor: null });

    await expect(getSegmentsPage()).rejects.toThrow('not a page payload');
  });
});

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

  it('uses one coherent status snapshot and preserves typed mutation results', async () => {
    const initialStatus = { undoAction: 'updateSegment' as const, redoAction: null };
    const undone = {
      action: 'updateSegment' as const,
      status: { undoAction: null, redoAction: 'updateSegment' as const },
    };
    const redone = {
      action: 'updateSegment' as const,
      status: { undoAction: 'updateSegment' as const, redoAction: null },
    };
    invokeMock
      .mockResolvedValueOnce(initialStatus)
      .mockResolvedValueOnce(undone)
      .mockResolvedValueOnce(redone);

    await expect(getHistoryStatusV1()).resolves.toEqual(initialStatus);
    await expect(undo()).resolves.toEqual(undone);
    await expect(redo()).resolves.toEqual(redone);
    expect(invokeMock.mock.calls).toEqual([['get_history_status_v1'], ['undo'], ['redo']]);
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
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(draft)
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(true);

    await expect(getReviewDraftV1(draft.segmentId)).resolves.toEqual(draft);
    await expect(
      saveReviewDraftV1(draft.segmentId, draft.baseRevision, draft.text),
    ).resolves.toEqual(draft);
    await expect(deleteReviewDraftV1(draft.segmentId, draft.baseRevision)).resolves.toBe(true);

    const saveOperationId = (invokeMock.mock.calls[1][1] as { operationId: string }).operationId;
    const deleteOperationId = (invokeMock.mock.calls[3][1] as { operationId: string }).operationId;
    const operationUuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
    expect(saveOperationId).toMatch(operationUuid);
    expect(deleteOperationId).toMatch(operationUuid);
    expect(deleteOperationId).not.toBe(saveOperationId);
    expect(invokeMock.mock.calls).toEqual([
      ['get_review_draft_v1', { segmentId: draft.segmentId }],
      [
        'reserve_review_draft_write_v1',
        { segmentId: draft.segmentId, operationId: saveOperationId },
      ],
      [
        'save_review_draft_v1',
        {
          segmentId: draft.segmentId,
          baseRevision: draft.baseRevision,
          text: draft.text,
          operationId: saveOperationId,
        },
      ],
      [
        'reserve_review_draft_write_v1',
        { segmentId: draft.segmentId, operationId: deleteOperationId },
      ],
      [
        'delete_review_draft_v1',
        {
          segmentId: draft.segmentId,
          baseRevision: draft.baseRevision,
          operationId: deleteOperationId,
        },
      ],
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
    invokeMock.mockResolvedValueOnce(null).mockRejectedValueOnce(refusal);

    await expect(saveReviewDraftV1('segment-draft', 9, 'text')).rejects.toBe(refusal);
    expect(invokeMock).toHaveBeenCalledTimes(2);
    const operationId = (invokeMock.mock.calls[0][1] as { operationId: string }).operationId;
    expect(invokeMock.mock.calls).toEqual([
      ['reserve_review_draft_write_v1', { segmentId: 'segment-draft', operationId }],
      [
        'save_review_draft_v1',
        { segmentId: 'segment-draft', baseRevision: 9, text: 'text', operationId },
      ],
    ]);
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
    const operationId = '70000000-0000-4000-8000-000000000052';
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

    await expect(
      recordReviewFlag({
        segmentId: 'segment-2',
        baseRevision: 7,
        rationale: 'needs a second listen',
        operationId,
      }),
    ).resolves.toBe(commit);

    expect(invokeMock).toHaveBeenCalledTimes(2);
    const first = invokeMock.mock.calls[0];
    const second = invokeMock.mock.calls[1];
    expect(first[0]).toBe('record_review_flag');
    expect(second[0]).toBe('record_review_flag');
    expect(second[1]).toEqual(first[1]);
    expect(first[1]).toMatchObject({
      request: {
        segmentId: 'segment-2',
        baseRevision: 7,
        rationale: 'needs a second listen',
        operationId,
      },
    });
  });
});
