import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type {
  AudioHealth,
  DatasetQuality,
  IntelligenceReport,
  SnapshotInfo,
  TrainingGradeBreakdown,
} from '../../src/lib/commands';
import { setLocale } from '../../src/lib/i18n';
import type {
  AccuracyRecord,
  InferenceStats,
  StatsBlocker,
} from '../../src/lib/statsDashboardModel';
import StatsQualitySection from '../../src/lib/StatsQualitySection.svelte';
import StatsReadinessSection from '../../src/lib/StatsReadinessSection.svelte';
import StatsRuntimeEvidence from '../../src/lib/StatsRuntimeEvidence.svelte';
import StatsToolsSection from '../../src/lib/StatsToolsSection.svelte';
import type { DatasetStats } from '../../src/lib/types';

function stats(overrides: Partial<DatasetStats> = {}): DatasetStats {
  return {
    totalSegments: 12,
    totalDurationSeconds: 3723,
    avgDurationSeconds: 310.25,
    verifiedCount: 6,
    pendingCount: 6,
    verificationRate: 50,
    uniqueSpeakers: 3,
    totalChars: 240,
    avgCharsPerSegment: 20,
    durationHistogram: { under5s: 1, under10s: 2, under15s: 3, under30s: 4, over30s: 2 },
    topSpeakers: [],
    reviewTiming: { decisionsLogged: 9, medianSeconds: 4.25, samples: 8 },
    dbSizeBytes: 2 * 1048576,
    ...overrides,
  };
}

function quality(overrides: Partial<DatasetQuality> = {}): DatasetQuality {
  return {
    totalSegments: 12,
    emptyTranscriptCount: 1,
    lowConfidenceCount: 2,
    duplicateTranscriptGroups: 4,
    duplicateTranscriptSegments: 9,
    durationOutlierCount: 3,
    medianDurationMs: 4000,
    q1DurationMs: 2000,
    q3DurationMs: 6000,
    duplicateGroups: [],
    durationOutliers: [],
    annotatedSegmentCount: 8,
    meanWer: 0.125,
    meanCer: 0.0625,
    segmentsAboveWerThreshold: 2,
    segmentsAboveCerThreshold: 0,
    qualityGatePassed: false,
    werOutliers: [],
    ...overrides,
  };
}

function inferenceStats(overrides: Partial<InferenceStats> = {}): InferenceStats {
  return {
    vad: { calls: 10, failures: 1, p50_ms: 0.5, p99_ms: 1200 },
    asr: { calls: 4, failures: 0, p50_ms: 12.3, p99_ms: 980 },
    model_load_ms: 2500,
    ...overrides,
  };
}

function intelligence(overrides: Partial<IntelligenceReport> = {}): IntelligenceReport {
  return {
    loop0Shadow: {
      totalObservations: 8,
      wouldFire: 3,
      firedButHumanAcceptedOriginal: 0,
      firedAndHumanEdited: 2,
      firedAndHumanRejected: 1,
    },
    autoAcceptPrecision: {
      t0Accepts: 5,
      t1Escalations: 2,
      t0HumanConfirmed: 3,
      t0HumanContradicted: 1,
    },
    conformalCalibration: {
      targetErrorCer: 0.05,
      perBucketDelta: 0.01,
      minNeededAtZeroCer: 20,
      buckets: [
        { bucket: 'high', verifiedWithReference: 7, minNeededAtZeroCer: 20 },
        { bucket: 'low', verifiedWithReference: 2, minNeededAtZeroCer: 20 },
      ],
    },
    ...overrides,
  };
}

function toolProps(
  overrides: Partial<{
    toolBusy: string | null;
    snapshots: SnapshotInfo[] | null;
    buildSha: string | null;
  }> = {},
) {
  return {
    toolBusy: null,
    snapshots: null,
    buildSha: null,
    onImportGold: vi.fn(),
    onExportGold: vi.fn(),
    onExportFinetune: vi.fn(),
    onBackup: vi.fn(),
    onRestoreFile: vi.fn(),
    onToggleSnapshots: vi.fn(),
    onRestoreSnapshot: vi.fn<(name: string, segmentCount: number | null) => void>(),
    onCompact: vi.fn(),
    ...overrides,
  };
}

beforeEach(async () => {
  await setLocale('en');
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe('StatsReadinessSection evidence and actions', () => {
  it('renders every blocker truth, accuracy provenance, owner metrics, and exact actions', async () => {
    const onRelink = vi.fn();
    const onOpenReview = vi.fn();
    const audioHealth: AudioHealth = {
      totalFiles: 5,
      missingFiles: 2,
      missingPaths: ['private-a.wav', 'private-b.wav'],
    };
    const breakdown: TrainingGradeBreakdown = {
      summary: {
        totalSegments: 12,
        trainingReadySegments: 1,
        goldSegments: 1,
        silverSegments: 0,
        reviewSegments: 8,
        rejectedSegments: 3,
      },
      reasonCounts: { human_verified: 1, pending: 8 },
    };
    const blockers: StatsBlocker[] = [
      { id: 'audioMissing', count: 2, action: 'relink' },
      { id: 'pendingReview', count: 6, action: 'review' },
      { id: 'emptyTranscripts', count: 1 },
      { id: 'qualityGate', count: 3 },
      { id: 'nothingTrainingReady', count: 8, detail: 'energy_heuristic_alignment' },
      { id: 'noAccuracyRecord' },
      { id: 'futureClosedReason', count: 4, detail: 'closed-detail' },
    ];
    const accuracy: AccuracyRecord = {
      run: {
        id: '12345678-rest-of-id',
        modelId: 'owner/champion',
        runAt: '2026-08-28T12:00:00Z',
        numSegs: 348,
        wer: 0.1234,
        cer: 0.0703,
      },
      cerLow: 0.06,
      cerHigh: 0.08,
      werLow: 0.11,
      werHigh: 0.14,
    };

    render(StatsReadinessSection, {
      props: {
        stats: stats(),
        audioHealth,
        breakdown,
        blockers,
        verdict: 'notReady',
        accuracy,
        evalRunsLoaded: true,
        fingerprintCount: 7,
        relinking: false,
        onRelink,
        onOpenReview,
      },
    });

    expect(screen.getByTestId('audio-missing-banner')).toHaveTextContent('2');
    expect(screen.getByTestId('readiness-headline')).toHaveTextContent('Not ready');
    expect(screen.getByTestId('readiness-count')).toHaveTextContent('1/12');
    expect(screen.getByTestId('readiness-blockers')).toHaveTextContent(
      'energy_heuristic_alignment',
    );
    expect(screen.getByTestId('readiness-blockers')).toHaveTextContent('futureClosedReason');
    expect(screen.getByTestId('readiness-next-action')).toHaveTextContent('2');
    expect(screen.getByTestId('accuracy-record')).toHaveTextContent('7.03%');
    expect(screen.getByTestId('accuracy-record')).toHaveTextContent('[6.00%–8.00%]');
    expect(screen.getByTestId('accuracy-record')).toHaveTextContent('12.34%');
    expect(screen.getByTestId('accuracy-record')).toHaveTextContent('owner/champion');
    expect(screen.getByTestId('accuracy-record')).toHaveTextContent('12345678');
    expect(screen.getByTestId('stat-review-speed')).toHaveTextContent('4.3s');
    expect(screen.getByTestId('stat-db-size')).toHaveTextContent('2.0 MB');
    expect(screen.getByTestId('stat-fingerprints')).toHaveTextContent('7');
    expect(screen.getByText('1h 2m')).toBeInTheDocument();

    await fireEvent.click(screen.getByTestId('relink-audio-btn'));
    await fireEvent.click(screen.getByTestId('blocker-relink-btn'));
    await fireEvent.click(screen.getByTestId('blocker-review-btn'));
    expect(onRelink).toHaveBeenCalledTimes(2);
    expect(onOpenReview).toHaveBeenCalledOnce();
  });

  it('renders a minimal ready result and honestly states that loaded evaluation has no record', () => {
    render(StatsReadinessSection, {
      props: {
        stats: stats({
          totalSegments: 0,
          totalDurationSeconds: 45,
          verifiedCount: 0,
          verificationRate: 0,
          uniqueSpeakers: 0,
          reviewTiming: { decisionsLogged: 0, medianSeconds: null, samples: 0 },
          dbSizeBytes: 0,
        }),
        audioHealth: { totalFiles: 0, missingFiles: 0, missingPaths: [] },
        breakdown: null,
        blockers: [],
        verdict: 'ready',
        accuracy: null,
        evalRunsLoaded: true,
        fingerprintCount: null,
        relinking: false,
        onRelink: vi.fn(),
      },
    });

    expect(screen.getByTestId('readiness-headline')).toHaveTextContent('Ready');
    expect(screen.getByTestId('accuracy-none')).toBeInTheDocument();
    expect(screen.queryByTestId('audio-missing-banner')).not.toBeInTheDocument();
    expect(screen.queryByTestId('readiness-count')).not.toBeInTheDocument();
    expect(screen.queryByTestId('readiness-blockers')).not.toBeInTheDocument();
    expect(screen.queryByTestId('stat-review-speed')).not.toBeInTheDocument();
    expect(screen.queryByTestId('stat-db-size')).not.toBeInTheDocument();
    expect(screen.queryByTestId('stat-fingerprints')).not.toBeInTheDocument();
    expect(screen.getByText('45s')).toBeInTheDocument();
  });

  it('keeps unknown evidence neutral, disables relinking, and never invents a review action', () => {
    const blockers: StatsBlocker[] = [
      { id: 'pendingReview', count: 1, action: 'review' },
      { id: 'unknownClosedReason' },
    ];
    const accuracy: AccuracyRecord = {
      run: {
        id: 'abcdefgh-rest',
        modelId: 'champion',
        runAt: 'now',
        numSegs: 1,
        wer: Number.NaN,
        cer: Number.NaN,
      },
      cerLow: 0.01,
      cerHigh: null,
      werLow: null,
      werHigh: 0.02,
    };

    render(StatsReadinessSection, {
      props: {
        stats: stats(),
        audioHealth: { totalFiles: 1, missingFiles: 1, missingPaths: ['private.wav'] },
        breakdown: null,
        blockers,
        verdict: 'unknown',
        accuracy,
        evalRunsLoaded: false,
        fingerprintCount: null,
        relinking: true,
        onRelink: vi.fn(),
      },
    });

    expect(screen.getByTestId('readiness-headline')).toHaveTextContent('Readiness unknown');
    expect(screen.getByTestId('relink-audio-btn')).toBeDisabled();
    expect(screen.getByTestId('relink-audio-btn')).toHaveTextContent('Relinking');
    expect(screen.queryByTestId('blocker-review-btn')).not.toBeInTheDocument();
    expect(screen.getByTestId('accuracy-record')).toHaveTextContent('—');
    expect(screen.getByTestId('accuracy-record')).not.toHaveTextContent('1.00%–');
    expect(screen.queryByTestId('accuracy-none')).not.toBeInTheDocument();
  });
});

describe('StatsRuntimeEvidence measured runtime states', () => {
  it('shows measured inference, positive precision, zero over-trigger failures, and bucket evidence', () => {
    render(StatsRuntimeEvidence, {
      props: { inferenceStats: inferenceStats(), intel: intelligence() },
    });

    expect(screen.getByText('10 calls')).toBeInTheDocument();
    expect(screen.getByText(/90\.0% ok/)).toBeInTheDocument();
    expect(screen.getByText(/500µs/)).toBeInTheDocument();
    expect(screen.getByText(/1\.20s/)).toBeInTheDocument();
    expect(screen.getByText(/Model load: 2\.50s/)).toBeInTheDocument();
    expect(screen.getByTestId('loop0-overtriggers')).toHaveTextContent('0');
    expect(screen.getByTestId('loop0-overtriggers')).toHaveClass('text-emerald-300');
    expect(screen.getByTestId('c4-precision')).toHaveTextContent('75%');
    expect(screen.getByTestId('conformal-progress')).toHaveTextContent('high: 7/20');
    expect(screen.getByTestId('conformal-progress')).toHaveTextContent('low: 2/20');
  });

  it('distinguishes no evidence from observed over-triggers and an unmeasured precision', () => {
    const noPrecision = intelligence({
      loop0Shadow: {
        totalObservations: 2,
        wouldFire: 0,
        firedButHumanAcceptedOriginal: 0,
        firedAndHumanEdited: 0,
        firedAndHumanRejected: 0,
      },
      autoAcceptPrecision: {
        t0Accepts: 1,
        t1Escalations: 0,
        t0HumanConfirmed: 0,
        t0HumanContradicted: 0,
      },
      conformalCalibration: null as unknown as IntelligenceReport['conformalCalibration'],
    });
    const view = render(StatsRuntimeEvidence, {
      props: {
        inferenceStats: inferenceStats({
          vad: { calls: 0, failures: 0, p50_ms: Number.NaN, p99_ms: 0 },
          asr: { calls: 0, failures: 0, p50_ms: 0, p99_ms: 0 },
          model_load_ms: 0,
        }),
        intel: noPrecision,
      },
    });

    expect(screen.getByTestId('loop0-overtriggers')).toHaveTextContent('—');
    expect(screen.getByTestId('loop0-overtriggers')).toHaveClass('text-cortex-400');
    expect(screen.getByTestId('c4-precision')).toHaveTextContent('—');
    expect(screen.getByTestId('intelligence-report')).toHaveTextContent('no evidence yet');
    expect(screen.queryByTestId('conformal-progress')).not.toBeInTheDocument();
    expect(screen.queryByText(/% ok/)).not.toBeInTheDocument();
    expect(screen.queryByText(/Model load:/)).not.toBeInTheDocument();

    view.unmount();
    render(StatsRuntimeEvidence, {
      props: {
        inferenceStats: null,
        intel: intelligence({
          loop0Shadow: {
            totalObservations: 3,
            wouldFire: 2,
            firedButHumanAcceptedOriginal: 1,
            firedAndHumanEdited: 1,
            firedAndHumanRejected: 0,
          },
        }),
      },
    });
    expect(screen.getByTestId('loop0-overtriggers')).toHaveTextContent('1');
    expect(screen.getByTestId('loop0-overtriggers')).toHaveClass('text-red-300');
  });

  it('hides an empty intelligence report instead of implying evidence', () => {
    render(StatsRuntimeEvidence, {
      props: {
        inferenceStats: null,
        intel: intelligence({
          loop0Shadow: {
            totalObservations: 0,
            wouldFire: 0,
            firedButHumanAcceptedOriginal: 0,
            firedAndHumanEdited: 0,
            firedAndHumanRejected: 0,
          },
          autoAcceptPrecision: {
            t0Accepts: 0,
            t1Escalations: 0,
            t0HumanConfirmed: 0,
            t0HumanContradicted: 0,
          },
        }),
      },
    });

    expect(screen.queryByTestId('intelligence-report')).not.toBeInTheDocument();
  });
});

describe('StatsToolsSection explicit owner tools', () => {
  it('routes every tool button to exactly its supplied action', async () => {
    const props = toolProps();
    render(StatsToolsSection, { props });

    const mappings = [
      ['import-verified-gold-btn', props.onImportGold],
      ['export-gold-eval-btn', props.onExportGold],
      ['export-finetune-pack-btn', props.onExportFinetune],
      ['backup-db-btn', props.onBackup],
      ['restore-file-btn', props.onRestoreFile],
      ['restore-snapshot-btn', props.onToggleSnapshots],
      ['compact-db-btn', props.onCompact],
    ] as const;
    for (const [testId, callback] of mappings) {
      await fireEvent.click(screen.getByTestId(testId));
      expect(callback).toHaveBeenCalledOnce();
    }
    expect(screen.queryByTestId('snapshot-list')).not.toBeInTheDocument();
    expect(screen.queryByTestId('build-sha')).not.toBeInTheDocument();
  });

  it('locks all tools while busy and renders explicit empty snapshot and build provenance', () => {
    const props = toolProps({ snapshots: [], toolBusy: 'backup', buildSha: 'abcdef1234567890' });
    render(StatsToolsSection, { props });

    for (const button of screen.getAllByRole('button')) expect(button).toBeDisabled();
    expect(screen.getByTestId('backup-db-btn')).toHaveTextContent('Working');
    expect(screen.getByTestId('snapshot-list')).toHaveTextContent('No auto-snapshots yet.');
    expect(screen.getByTestId('build-sha')).toHaveTextContent('abcdef123456');
  });

  it('preserves opaque snapshot names and forwards nullable segment authority on restore', async () => {
    const snapshots: SnapshotInfo[] = [
      {
        name: 'opaque-owner-snapshot-a',
        timestamp: 1_700_000_000,
        dbSizeBytes: 1048576,
        segmentCount: null,
      },
      {
        name: 'opaque-owner-snapshot-b',
        timestamp: 1_700_000_100,
        dbSizeBytes: 2621440,
        segmentCount: 42,
      },
    ];
    const props = toolProps({ snapshots });
    render(StatsToolsSection, { props });

    expect(screen.getByTestId('snapshot-list')).toHaveTextContent('opaque-owner-snapshot-a');
    expect(screen.getByTestId('snapshot-list')).toHaveTextContent('? seg');
    expect(screen.getByTestId('snapshot-list')).toHaveTextContent('42 seg');
    expect(screen.getByTestId('snapshot-list')).toHaveTextContent('2.5 MB');
    const restoreButtons = screen.getAllByRole('button', { name: 'Restore' });
    await fireEvent.click(restoreButtons[0]);
    await fireEvent.click(restoreButtons[1]);
    expect(props.onRestoreSnapshot).toHaveBeenNthCalledWith(1, 'opaque-owner-snapshot-a', null);
    expect(props.onRestoreSnapshot).toHaveBeenNthCalledWith(2, 'opaque-owner-snapshot-b', 42);
  });

  it('shows snapshot restore settlement as busy and blocks duplicate restore', () => {
    const props = toolProps({
      toolBusy: 'restore',
      snapshots: [
        {
          name: 'opaque-owner-snapshot',
          timestamp: 1_700_000_000,
          dbSizeBytes: 1,
          segmentCount: 1,
        },
      ],
    });
    render(StatsToolsSection, { props });

    const restore = screen.getAllByRole('button', { name: 'Working…' });
    expect(restore).toHaveLength(1);
    expect(restore[0]).toBeDisabled();
  });
});

describe('StatsQualitySection measured dataset quality', () => {
  it('hides absent and zero-sized quality reports', () => {
    const view = render(StatsQualitySection, { props: { quality: null } });
    expect(screen.queryByRole('heading', { name: 'Dataset Quality' })).not.toBeInTheDocument();
    view.unmount();

    render(StatsQualitySection, { props: { quality: quality({ totalSegments: 0 }) } });
    expect(screen.queryByRole('heading', { name: 'Dataset Quality' })).not.toBeInTheDocument();
  });

  it('renders measured quality, a failed gate, and only the first three duplicate groups', () => {
    render(StatsQualitySection, {
      props: {
        quality: quality({
          duplicateGroups: [
            { transcriptHash: '1', segmentIds: ['a', 'b'], normalizedPreview: 'first duplicate' },
            { transcriptHash: '2', segmentIds: ['c'], normalizedPreview: 'second duplicate' },
            {
              transcriptHash: '3',
              segmentIds: ['d', 'e', 'f'],
              normalizedPreview: 'third duplicate',
            },
            { transcriptHash: '4', segmentIds: ['g'], normalizedPreview: 'hidden duplicate' },
          ],
        }),
      },
    });

    expect(screen.getByText('12.5%')).toHaveClass('text-red-300');
    expect(screen.getByText('6.3%')).toHaveClass('text-emerald-300');
    expect(screen.getByText(/Quality gates failed/)).toBeInTheDocument();
    expect(screen.getByText(/first duplicate/)).toBeInTheDocument();
    expect(screen.getByText(/third duplicate/)).toBeInTheDocument();
    expect(screen.queryByText(/hidden duplicate/)).not.toBeInTheDocument();
  });

  it('shows honest unknown means and no failure claim for a passing gate', () => {
    render(StatsQualitySection, {
      props: {
        quality: quality({
          annotatedSegmentCount: 2,
          meanWer: null,
          meanCer: null,
          segmentsAboveWerThreshold: 0,
          segmentsAboveCerThreshold: 0,
          qualityGatePassed: true,
          duplicateGroups: [],
        }),
      },
    });

    expect(screen.getAllByText('—')).toHaveLength(2);
    expect(screen.getAllByText('—')[0]).toHaveClass('text-emerald-300');
    expect(screen.queryByText(/Quality gates failed/)).not.toBeInTheDocument();
  });

  it('does not invent annotation metrics when no reference annotations exist', () => {
    render(StatsQualitySection, {
      props: { quality: quality({ annotatedSegmentCount: 0, duplicateGroups: [] }) },
    });

    expect(screen.queryByText('Mean WER')).not.toBeInTheDocument();
    expect(screen.queryByText('Mean CER')).not.toBeInTheDocument();
  });
});
