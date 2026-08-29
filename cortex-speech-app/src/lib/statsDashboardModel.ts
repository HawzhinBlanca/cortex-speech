import type { AudioHealth, DatasetQuality, TrainingGradeBreakdown } from './commands';
import {
  effectiveTranscript,
  isHumanRejected,
  isPlaceholderTranscript,
  isVerifiedGood,
} from './segmentQuality';
import type { DatasetStats, EvalRun, SpeechSegment } from './types';

export type StatsBlocker = {
  id: string;
  count?: number;
  detail?: string;
  action?: 'relink' | 'review';
};

export type AccuracyRecord = {
  run: EvalRun;
  cerLow: number | null;
  cerHigh: number | null;
  werLow: number | null;
  werHigh: number | null;
};

export type InferenceStats = {
  vad: { calls: number; failures: number; p50_ms: number; p99_ms: number };
  asr: { calls: number; failures: number; p50_ms: number; p99_ms: number };
  model_load_ms: number;
};

export function buildStatsBlockers(
  audioHealth: AudioHealth | null,
  stats: DatasetStats | null,
  quality: DatasetQuality | null,
  breakdown: TrainingGradeBreakdown | null,
  evalRuns: EvalRun[] | null,
): StatsBlocker[] {
  const blockers: StatsBlocker[] = [];
  if (audioHealth && audioHealth.missingFiles > 0) {
    blockers.push({ id: 'audioMissing', count: audioHealth.missingFiles, action: 'relink' });
  }
  if (stats && stats.pendingCount > 0) {
    blockers.push({ id: 'pendingReview', count: stats.pendingCount, action: 'review' });
  }
  if (quality && quality.emptyTranscriptCount > 0) {
    blockers.push({ id: 'emptyTranscripts', count: quality.emptyTranscriptCount });
  }
  if (quality && !quality.qualityGatePassed) {
    blockers.push({
      id: 'qualityGate',
      count: quality.segmentsAboveWerThreshold + quality.segmentsAboveCerThreshold,
    });
  }
  if (
    breakdown &&
    breakdown.summary.totalSegments > 0 &&
    breakdown.summary.trainingReadySegments === 0
  ) {
    const dominantReason = Object.entries(breakdown.reasonCounts)
      .filter(([reason]) => reason !== 'human_verified')
      .sort((left, right) => right[1] - left[1])[0];
    blockers.push({
      id: 'nothingTrainingReady',
      detail: dominantReason?.[0],
      count: dominantReason?.[1],
    });
  }
  if (evalRuns !== null && evalRuns.length === 0) {
    blockers.push({ id: 'noAccuracyRecord' });
  }
  return blockers;
}

export function readAccuracyRecord(evalRuns: EvalRun[] | null): AccuracyRecord | null {
  const run = evalRuns?.[0];
  if (!run) return null;

  let cerLow: number | null = null;
  let cerHigh: number | null = null;
  let werLow: number | null = null;
  let werHigh: number | null = null;
  try {
    const meta: unknown = run.metaJson ? JSON.parse(run.metaJson) : null;
    if (meta && typeof meta === 'object') {
      const record = meta as Record<string, unknown>;
      cerLow = typeof record.micro_cer_ci_low === 'number' ? record.micro_cer_ci_low : null;
      cerHigh = typeof record.micro_cer_ci_high === 'number' ? record.micro_cer_ci_high : null;
      werLow = typeof record.micro_wer_ci_low === 'number' ? record.micro_wer_ci_low : null;
      werHigh = typeof record.micro_wer_ci_high === 'number' ? record.micro_wer_ci_high : null;
    }
  } catch {
    // The point estimate is still valid when legacy metadata is malformed; no interval is claimed.
  }
  return { run, cerLow, cerHigh, werLow, werHigh };
}

export function buildLocalStats(items: SpeechSegment[]): DatasetStats {
  const durationSeconds = items.map((segment) => Math.max(0, segment.durationMs || 0) / 1000);
  const totalDurationSeconds = durationSeconds.reduce((sum, value) => sum + value, 0);
  const verifiedCount = items.filter((segment) => isVerifiedGood(segment)).length;
  const countedForChars = items.filter(
    (segment) =>
      !isHumanRejected(segment) && !isPlaceholderTranscript(effectiveTranscript(segment)),
  );
  const totalChars = countedForChars.reduce(
    (sum, segment) => sum + effectiveTranscript(segment).length,
    0,
  );
  const speakerDurations = new Map<
    string,
    { segmentCount: number; totalDurationSeconds: number }
  >();

  for (const segment of items) {
    const speakerId = segment.speakerId || 'unknown';
    const current = speakerDurations.get(speakerId) ?? {
      segmentCount: 0,
      totalDurationSeconds: 0,
    };
    current.segmentCount += 1;
    current.totalDurationSeconds += Math.max(0, segment.durationMs || 0) / 1000;
    speakerDurations.set(speakerId, current);
  }

  return {
    totalSegments: items.length,
    totalDurationSeconds,
    avgDurationSeconds: items.length ? totalDurationSeconds / items.length : 0,
    verifiedCount,
    pendingCount: items.filter((segment) => !isVerifiedGood(segment) && !isHumanRejected(segment))
      .length,
    verificationRate: items.length ? (verifiedCount / items.length) * 100 : 0,
    uniqueSpeakers: speakerDurations.size,
    totalChars,
    avgCharsPerSegment: countedForChars.length ? totalChars / countedForChars.length : 0,
    durationHistogram: {
      under5s: durationSeconds.filter((duration) => duration < 5).length,
      under10s: durationSeconds.filter((duration) => duration >= 5 && duration < 10).length,
      under15s: durationSeconds.filter((duration) => duration >= 10 && duration < 15).length,
      under30s: durationSeconds.filter((duration) => duration >= 15 && duration < 30).length,
      over30s: durationSeconds.filter((duration) => duration >= 30).length,
    },
    topSpeakers: Array.from(speakerDurations.entries())
      .map(([speakerId, value]) => ({ speakerId, ...value }))
      .sort(
        (left, right) =>
          right.segmentCount - left.segmentCount ||
          right.totalDurationSeconds - left.totalDurationSeconds,
      )
      .slice(0, 5),
    reviewTiming: { decisionsLogged: 0, medianSeconds: null, samples: 0 },
    dbSizeBytes: 0,
  };
}

export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds)) return '\u2014';
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const remainder = Math.floor(seconds % 60);
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${remainder}s`;
  return `${remainder}s`;
}

export function formatPercent(value: number): string {
  return Number.isFinite(value) ? `${value.toFixed(1)}%` : '\u2014';
}

export function formatRatePercent(value: number): string {
  return Number.isFinite(value) ? `${(value * 100).toFixed(2)}%` : '\u2014';
}

export function formatMilliseconds(value: number): string {
  if (!Number.isFinite(value)) return '\u2014';
  if (value < 1) return `${(value * 1000).toFixed(0)}\u00b5s`;
  if (value < 1000) return `${value.toFixed(1)}ms`;
  return `${(value / 1000).toFixed(2)}s`;
}
