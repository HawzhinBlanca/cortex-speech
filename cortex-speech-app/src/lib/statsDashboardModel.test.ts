import { describe, expect, it } from 'vitest';
import type { AudioHealth, DatasetQuality, TrainingGradeBreakdown } from './commands';
import {
  buildLocalStats,
  buildStatsBlockers,
  formatDuration,
  formatMilliseconds,
  formatPercent,
  formatRatePercent,
  readAccuracyRecord,
} from './statsDashboardModel';
import type { DatasetStats, EvalRun, SpeechSegment } from './types';

function segment(overrides: Partial<SpeechSegment> = {}): SpeechSegment {
  return {
    id: 'segment-1',
    audioPath: 'C:\\audio\\sample.wav',
    rawTranscript: 'raw',
    normalizedTranscript: null,
    annotatedTranscript: null,
    alignmentJson: null,
    durationMs: 1_000,
    speakerId: null,
    verified: false,
    ...overrides,
  };
}

function datasetStats(overrides: Partial<DatasetStats> = {}): DatasetStats {
  return {
    totalSegments: 10,
    totalDurationSeconds: 10,
    avgDurationSeconds: 1,
    verifiedCount: 7,
    pendingCount: 3,
    verificationRate: 70,
    uniqueSpeakers: 2,
    totalChars: 100,
    avgCharsPerSegment: 10,
    durationHistogram: { under5s: 10, under10s: 0, under15s: 0, under30s: 0, over30s: 0 },
    topSpeakers: [],
    reviewTiming: { decisionsLogged: 0, medianSeconds: null, samples: 0 },
    dbSizeBytes: 0,
    ...overrides,
  };
}

function quality(overrides: Partial<DatasetQuality> = {}): DatasetQuality {
  return {
    totalSegments: 10,
    emptyTranscriptCount: 1,
    lowConfidenceCount: 0,
    duplicateTranscriptGroups: 0,
    duplicateTranscriptSegments: 0,
    durationOutlierCount: 0,
    medianDurationMs: 1_000,
    q1DurationMs: 800,
    q3DurationMs: 1_200,
    duplicateGroups: [],
    durationOutliers: [],
    annotatedSegmentCount: 0,
    meanWer: null,
    meanCer: null,
    segmentsAboveWerThreshold: 4,
    segmentsAboveCerThreshold: 5,
    qualityGatePassed: false,
    werOutliers: [],
    ...overrides,
  };
}

function breakdown(overrides: Partial<TrainingGradeBreakdown> = {}): TrainingGradeBreakdown {
  return {
    summary: {
      totalSegments: 10,
      trainingReadySegments: 0,
      goldSegments: 0,
      silverSegments: 0,
      reviewSegments: 10,
      rejectedSegments: 0,
    },
    reasonCounts: {
      human_verified: 10,
      missing_alignment: 2,
      placeholder_transcript: 3,
    },
    ...overrides,
  };
}

function evalRun(overrides: Partial<EvalRun> = {}): EvalRun {
  return {
    id: 'eval-1',
    modelId: 'omniasr-wsl-7b',
    runAt: '2026-08-28T00:00:00Z',
    numSegs: 348,
    wer: 0.12,
    cer: 0.07,
    metaJson: null,
    ...overrides,
  };
}

describe('statsDashboardModel evidence truth', () => {
  it('reports every concrete blocker and selects the dominant non-human training reason', () => {
    const audioHealth: AudioHealth = { totalFiles: 10, missingFiles: 2, missingPaths: [] };

    expect(buildStatsBlockers(audioHealth, datasetStats(), quality(), breakdown(), [])).toEqual([
      { id: 'audioMissing', count: 2, action: 'relink' },
      { id: 'pendingReview', count: 3, action: 'review' },
      { id: 'emptyTranscripts', count: 1 },
      { id: 'qualityGate', count: 9 },
      { id: 'nothingTrainingReady', detail: 'placeholder_transcript', count: 3 },
      { id: 'noAccuracyRecord' },
    ]);
  });

  it('does not invent blockers while evidence is unavailable or training output exists', () => {
    expect(buildStatsBlockers(null, null, null, null, null)).toEqual([]);
    expect(
      buildStatsBlockers(
        { totalFiles: 1, missingFiles: 0, missingPaths: [] },
        datasetStats({ pendingCount: 0 }),
        quality({ emptyTranscriptCount: 0, qualityGatePassed: true }),
        breakdown({
          summary: {
            totalSegments: 10,
            trainingReadySegments: 8,
            goldSegments: 2,
            silverSegments: 6,
            reviewSegments: 2,
            rejectedSegments: 0,
          },
        }),
        [evalRun()],
      ),
    ).toEqual([]);
  });

  it('accepts only numeric confidence intervals from evaluation metadata', () => {
    const run = evalRun({
      metaJson: JSON.stringify({
        micro_cer_ci_low: 0.06,
        micro_cer_ci_high: 0.08,
        micro_wer_ci_low: '0.10',
        micro_wer_ci_high: 0.14,
      }),
    });

    expect(readAccuracyRecord([run])).toEqual({
      run,
      cerLow: 0.06,
      cerHigh: 0.08,
      werLow: null,
      werHigh: 0.14,
    });
  });

  it('keeps the point estimate but claims no interval for malformed or absent metadata', () => {
    const malformed = evalRun({ metaJson: '{not-json' });
    expect(readAccuracyRecord([malformed])).toEqual({
      run: malformed,
      cerLow: null,
      cerHigh: null,
      werLow: null,
      werHigh: null,
    });
    expect(readAccuracyRecord([])).toBeNull();
    expect(readAccuracyRecord(null)).toBeNull();
  });

  it('derives local totals from effective human truth and excludes rejected and placeholder text', () => {
    const result = buildLocalStats([
      segment({
        id: 'accepted-edit',
        rawTranscript: 'stale draft',
        verdictTranscript: ' final truth ',
        humanDecision: 'edit',
        verified: true,
        durationMs: 4_999,
        speakerId: 'alice',
      }),
      segment({
        id: 'rejected',
        rawTranscript: 'must not count',
        humanDecision: 'reject',
        verified: true,
        durationMs: 5_000,
        speakerId: 'alice',
      }),
      segment({
        id: 'placeholder',
        rawTranscript: '[Pending WSL 7B ASR]',
        durationMs: 10_000,
      }),
      segment({
        id: 'annotation',
        rawTranscript: 'old',
        normalizedTranscript: 'normalized',
        annotatedTranscript: 'annotation',
        durationMs: -1_000,
        speakerId: 'bob',
      }),
      segment({
        id: 'normalized-evidence',
        rawTranscript: 'old',
        normalizedTranscript: 'norm',
        durationMs: 15_000,
        speakerId: 'bob',
      }),
      segment({
        id: 'raw',
        rawTranscript: 'raw',
        durationMs: 30_000,
        speakerId: 'carol',
      }),
    ]);

    expect(result).toMatchObject({
      totalSegments: 6,
      totalDurationSeconds: 64.999,
      avgDurationSeconds: 64.999 / 6,
      verifiedCount: 1,
      pendingCount: 4,
      uniqueSpeakers: 4,
      // Verbatim Law: the normalized `norm` is evidence only, so this row contributes raw `old`.
      totalChars: 27,
      avgCharsPerSegment: 6.75,
      durationHistogram: { under5s: 2, under10s: 1, under15s: 1, under30s: 1, over30s: 1 },
      reviewTiming: { decisionsLogged: 0, medianSeconds: null, samples: 0 },
      dbSizeBytes: 0,
    });
    expect(result.verificationRate).toBeCloseTo(100 / 6, 12);
    expect(
      result.topSpeakers.map(({ speakerId, segmentCount }) => ({ speakerId, segmentCount })),
    ).toEqual([
      { speakerId: 'bob', segmentCount: 2 },
      { speakerId: 'alice', segmentCount: 2 },
      { speakerId: 'carol', segmentCount: 1 },
      { speakerId: 'unknown', segmentCount: 1 },
    ]);
    expect(result.topSpeakers[0].totalDurationSeconds).toBe(15);
    expect(result.topSpeakers[1].totalDurationSeconds).toBeCloseTo(9.999, 12);
  });

  it('returns honest zero denominators and caps the speaker ranking at five', () => {
    expect(buildLocalStats([])).toEqual({
      totalSegments: 0,
      totalDurationSeconds: 0,
      avgDurationSeconds: 0,
      verifiedCount: 0,
      pendingCount: 0,
      verificationRate: 0,
      uniqueSpeakers: 0,
      totalChars: 0,
      avgCharsPerSegment: 0,
      durationHistogram: { under5s: 0, under10s: 0, under15s: 0, under30s: 0, over30s: 0 },
      topSpeakers: [],
      reviewTiming: { decisionsLogged: 0, medianSeconds: null, samples: 0 },
      dbSizeBytes: 0,
    });

    const ranked = buildLocalStats(
      ['a', 'b', 'c', 'd', 'e', 'f'].map((speakerId, index) =>
        segment({ id: speakerId, speakerId, durationMs: (index + 1) * 1_000 }),
      ),
    ).topSpeakers;
    expect(ranked).toHaveLength(5);
    expect(ranked.map((speaker) => speaker.speakerId)).toEqual(['f', 'e', 'd', 'c', 'b']);
  });

  it('formats only finite metrics and preserves unit boundaries', () => {
    expect(formatDuration(Number.POSITIVE_INFINITY)).toBe('—');
    expect(formatDuration(3_661)).toBe('1h 1m');
    expect(formatDuration(61)).toBe('1m 1s');
    expect(formatDuration(0.9)).toBe('0s');
    expect(formatPercent(12.34)).toBe('12.3%');
    expect(formatPercent(Number.NaN)).toBe('—');
    expect(formatRatePercent(0.0703)).toBe('7.03%');
    expect(formatRatePercent(Number.NEGATIVE_INFINITY)).toBe('—');
    expect(formatMilliseconds(0.5)).toBe('500µs');
    expect(formatMilliseconds(12.34)).toBe('12.3ms');
    expect(formatMilliseconds(1_500)).toBe('1.50s');
    expect(formatMilliseconds(Number.NaN)).toBe('—');
  });
});
