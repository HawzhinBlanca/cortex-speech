import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke, type InvokeArgs } from '@tauri-apps/api/core';
import StatsDashboard from '../../src/lib/StatsDashboard.svelte';
import { chooseDirectory, chooseFile } from '../../src/lib/fileDialogs';
import { reloadApp } from '../../src/lib/reloadBoundary';
import { setLocale } from '../../src/lib/i18n';
import { notifications } from '../../src/lib/stores/notificationStore';
import { segments } from '../../src/lib/stores/segmentStore';
// Two desktop-tools cases render the whole dashboard through jsdom. Measured 2026-09-02 on the hosted
// Windows runner (PR #80, run 33672342596): both timed out at vitest's default 5 s while the runner's
// environment setup alone took 157 s; locally the pair completes in about a second. A per-case budget,
// not a global one: only these renders are that heavy, and a stuck test must still fail.
const DESKTOP_RENDER_BUDGET_MS = 20_000;

vi.mock('../../src/lib/fileDialogs', () => ({
  chooseDirectory: vi.fn(),
  chooseFile: vi.fn(),
}));

vi.mock('../../src/lib/reloadBoundary', () => ({
  reloadApp: vi.fn(),
}));

const invokeMock = vi.mocked(invoke);
const chooseDirectoryMock = vi.mocked(chooseDirectory);
const chooseFileMock = vi.mocked(chooseFile);
const reloadAppMock = vi.mocked(reloadApp);

const datasetStats = {
  totalSegments: 4,
  totalDurationSeconds: 80,
  avgDurationSeconds: 20,
  verifiedCount: 3,
  pendingCount: 1,
  verificationRate: 75,
  uniqueSpeakers: 2,
  totalChars: 100,
  avgCharsPerSegment: 25,
  durationHistogram: { under5s: 0, under10s: 1, under15s: 2, under30s: 1, over30s: 0 },
  topSpeakers: [],
  reviewTiming: { decisionsLogged: 3, medianSeconds: 6, samples: 2 },
  dbSizeBytes: 1048576,
};

const datasetQuality = {
  totalSegments: 4,
  emptyTranscriptCount: 0,
  lowConfidenceCount: 0,
  duplicateTranscriptGroups: 0,
  duplicateTranscriptSegments: 0,
  durationOutlierCount: 0,
  medianDurationMs: 20000,
  q1DurationMs: 10000,
  q3DurationMs: 30000,
  duplicateGroups: [],
  durationOutliers: [],
  annotatedSegmentCount: 0,
  meanWer: null,
  meanCer: null,
  segmentsAboveWerThreshold: 0,
  segmentsAboveCerThreshold: 0,
  qualityGatePassed: true,
  werOutliers: [],
};

const certificate = {
  targetError: 0.05,
  confidenceLevel: 0.95,
  threshold: 0.4,
  totalCertified: 2,
  certifiedSegmentIds: ['seg-a', 'seg-b'],
  expectedErrorBound: 0.04,
  isCalibrated: true,
  calibrationRealPosterior: 0,
  calibrationHeuristic: 2,
  calibrationNoConfidence: 0,
};

const breakdown = {
  summary: {
    totalSegments: 4,
    trainingReadySegments: 3,
    goldSegments: 1,
    silverSegments: 2,
    reviewSegments: 1,
    rejectedSegments: 0,
  },
  reasonCounts: { human_verified: 3 },
};

const inferenceStats = {
  vad: { calls: 2, failures: 0, p50_ms: 1, p99_ms: 2 },
  asr: { calls: 2, failures: 0, p50_ms: 10, p99_ms: 20 },
  model_load_ms: 100,
};

const intelligence = {
  loop0Shadow: {
    totalObservations: 1,
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
  conformalCalibration: {
    targetErrorCer: 0.05,
    perBucketDelta: 0.01,
    minNeededAtZeroCer: 20,
    buckets: [],
  },
};

function successfulInvoke(command: string, args?: InvokeArgs): Promise<unknown> {
  switch (command) {
    case 'get_dataset_stats':
      return Promise.resolve(datasetStats);
    case 'get_dataset_quality':
      return Promise.resolve(datasetQuality);
    case 'get_fingerprint_count':
      return Promise.resolve(4);
    case 'get_dataset_certificate':
      return Promise.resolve(certificate);
    case 'get_audio_health':
      return Promise.resolve({ totalFiles: 4, missingFiles: 1, missingPaths: ['private.wav'] });
    case 'get_training_grade_breakdown':
      return Promise.resolve(breakdown);
    case 'list_eval_runs':
      return Promise.resolve([
        {
          id: 'eval-run-owner',
          modelId: 'owner/champion',
          runAt: '2026-08-28T12:00:00Z',
          numSegs: 4,
          wer: 0.1,
          cer: 0.05,
          metaJson: null,
        },
      ]);
    case 'get_inference_stats':
      return Promise.resolve(inferenceStats);
    case 'app_git_sha':
      return Promise.resolve('abcdef1234567890');
    case 'get_intelligence_report':
      return Promise.resolve(intelligence);
    case 'relink_audio':
      return Promise.resolve({ relinked: 1, stillMissing: 0 });
    case 'import_verified_segments_as_gold':
      return Promise.resolve(3);
    case 'export_gold_eval_set':
      return Promise.resolve({
        manifestPath: 'manifest.jsonl',
        totalGold: 4,
        exported: 3,
        skipped: 1,
      });
    case 'export_finetune_pack':
      return Promise.resolve({
        manifestPath: 'finetune.jsonl',
        manifestSha256: 'sha',
        totalVerified: 4,
        excludedUnexportable: 1,
        excludedNotTrainingReady: 1,
        emitted: 2,
        skipped: 0,
        emittedWithoutHumanDecision: 0,
        snapshotId: 'snapshot',
        newlySealed: true,
      });
    case 'db_backup':
      return Promise.resolve({ integrityOk: true, segmentCount: 4 });
    case 'db_vacuum':
    case 'db_restore':
    case 'restore_db_from_snapshot':
      return Promise.resolve(null);
    case 'list_db_snapshots':
      return Promise.resolve([
        {
          name: 'opaque-owner-snapshot',
          timestamp: 1_700_000_000,
          dbSizeBytes: 1048576,
          segmentCount: 4,
        },
      ]);
    default:
      return Promise.reject(new Error(`Unexpected command: ${command} ${JSON.stringify(args)}`));
  }
}

async function renderDesktop(onOpenReview = vi.fn()) {
  const view = render(StatsDashboard, { props: { onOpenReview } });
  expect(await screen.findByTestId('readiness-headline')).toHaveTextContent('Not ready');
  return { view, onOpenReview };
}

beforeEach(async () => {
  invokeMock.mockReset();
  invokeMock.mockImplementation(successfulInvoke);
  chooseDirectoryMock.mockReset().mockResolvedValue('C:\\owner-backups');
  chooseFileMock.mockReset().mockResolvedValue('C:\\owner-backups\\backup.db');
  reloadAppMock.mockReset();
  notifications.clear();
  segments.set([]);
  window.__TAURI__ = {};
  delete window.__TAURI_INTERNALS__;
  vi.spyOn(window, 'confirm').mockReturnValue(true);
  vi.spyOn(console, 'error').mockImplementation(() => {});
  await setLocale('en');
});

afterEach(() => {
  cleanup();
  notifications.clear();
  segments.set([]);
  delete window.__TAURI__;
  delete window.__TAURI_INTERNALS__;
  vi.restoreAllMocks();
});

describe('StatsDashboard desktop evidence orchestration', () => {
  it('loads every authority, derives a fail-closed verdict, and exposes build/runtime evidence', async () => {
    const { onOpenReview } = await renderDesktop();

    expect(screen.getByTestId('readiness-count')).toHaveTextContent('3/4');
    expect(screen.getByTestId('accuracy-record')).toHaveTextContent('owner/champion');
    expect(screen.getByTestId('stat-fingerprints')).toHaveTextContent('4');
    expect(screen.getByTestId('build-sha')).toHaveTextContent('abcdef123456');
    expect(screen.getByTestId('intelligence-report')).toBeInTheDocument();
    expect(screen.getByTestId('audio-missing-banner')).toBeInTheDocument();
    await fireEvent.click(screen.getByTestId('blocker-review-btn'));
    expect(onOpenReview).toHaveBeenCalledOnce();

    await waitFor(() => {
      for (const command of [
        'get_dataset_stats',
        'get_dataset_quality',
        'get_fingerprint_count',
        'get_dataset_certificate',
        'get_audio_health',
        'get_training_grade_breakdown',
        'list_eval_runs',
        'get_inference_stats',
        'app_git_sha',
        'get_intelligence_report',
      ]) {
        expect(invokeMock.mock.calls.some(([seen]) => seen === command)).toBe(true);
      }
    });
    expect(invokeMock).toHaveBeenCalledWith('get_dataset_certificate', {
      targetError: 0.05,
      confidenceLevel: 0.95,
    });
  });

  it('runs relink, export, backup, import, compact, and snapshot-list workflows with exact arguments', { timeout: DESKTOP_RENDER_BUDGET_MS }, async () => {
    await renderDesktop();

    await fireEvent.click(screen.getByTestId('relink-audio-btn'));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith('relink_audio', { searchDir: 'C:\\owner-backups' }),
    );
    await waitFor(() => expect(screen.getByTestId('relink-audio-btn')).not.toBeDisabled());

    await fireEvent.click(screen.getByTestId('export-gold-eval-btn'));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith('export_gold_eval_set', {
        outDir: 'C:\\owner-backups',
      }),
    );
    await waitFor(() => expect(screen.getByTestId('export-gold-eval-btn')).not.toBeDisabled());

    await fireEvent.click(screen.getByTestId('export-finetune-pack-btn'));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith('export_finetune_pack', {
        outDir: 'C:\\owner-backups',
      }),
    );
    await waitFor(() => expect(screen.getByTestId('export-finetune-pack-btn')).not.toBeDisabled());
    await waitFor(() =>
      expect(get(notifications).some((item) => item.message.includes('Fine-tune pack'))).toBe(true),
    );

    await fireEvent.click(screen.getByTestId('backup-db-btn'));
    await waitFor(() => {
      const call = invokeMock.mock.calls.find(([command]) => command === 'db_backup');
      expect(call?.[1]).toMatchObject({
        dest: expect.stringMatching(
          /^C:\\owner-backups\\cortex-speech-backup-\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}\.db$/,
        ),
      });
    });
    await waitFor(() => expect(screen.getByTestId('backup-db-btn')).not.toBeDisabled());

    await fireEvent.click(screen.getByTestId('import-verified-gold-btn'));
    await waitFor(() =>
      expect(
        invokeMock.mock.calls.some(([command]) => command === 'import_verified_segments_as_gold'),
      ).toBe(true),
    );
    await waitFor(() => expect(screen.getByTestId('import-verified-gold-btn')).not.toBeDisabled());

    await fireEvent.click(screen.getByTestId('compact-db-btn'));
    await waitFor(() =>
      expect(invokeMock.mock.calls.some(([command]) => command === 'db_vacuum')).toBe(true),
    );
    await waitFor(() => expect(screen.getByTestId('compact-db-btn')).not.toBeDisabled());

    await fireEvent.click(screen.getByTestId('restore-snapshot-btn'));
    expect(await screen.findByText('opaque-owner-snapshot')).toBeInTheDocument();
    await fireEvent.click(screen.getByTestId('restore-snapshot-btn'));
    await waitFor(() =>
      expect(screen.queryByText('opaque-owner-snapshot')).not.toBeInTheDocument(),
    );

    expect(get(notifications).some((item) => item.type === 'error')).toBe(false);
    expect(get(notifications).filter((item) => item.type === 'success').length).toBeGreaterThan(4);
  });

  it('treats picker cancellation and tool failures as settled, retryable UI states', async () => {
    await renderDesktop();

    chooseDirectoryMock.mockResolvedValueOnce(null);
    await fireEvent.click(screen.getByTestId('export-gold-eval-btn'));
    await waitFor(() => expect(screen.getByTestId('export-gold-eval-btn')).not.toBeDisabled());
    expect(invokeMock.mock.calls.some(([command]) => command === 'export_gold_eval_set')).toBe(
      false,
    );

    chooseDirectoryMock.mockRejectedValueOnce(new Error('dialog unavailable'));
    await fireEvent.click(screen.getByTestId('export-finetune-pack-btn'));
    await waitFor(() => expect(get(notifications).at(-1)?.type).toBe('error'));
    expect(screen.getByTestId('export-finetune-pack-btn')).not.toBeDisabled();

    invokeMock.mockImplementation((command, args) => {
      if (command === 'import_verified_segments_as_gold' || command === 'db_vacuum') {
        return Promise.reject(new Error(`${command} refused`));
      }
      return successfulInvoke(command, args);
    });
    await fireEvent.click(screen.getByTestId('import-verified-gold-btn'));
    await waitFor(() => expect(screen.getByTestId('import-verified-gold-btn')).not.toBeDisabled());
    await fireEvent.click(screen.getByTestId('compact-db-btn'));
    await waitFor(() => expect(screen.getByTestId('compact-db-btn')).not.toBeDisabled());
    expect(
      get(notifications).filter((item) => item.type === 'error').length,
    ).toBeGreaterThanOrEqual(3);
  });

  it('requires confirmation for destructive restores and preserves retry after each refusal', async () => {
    await renderDesktop();
    await fireEvent.click(screen.getByTestId('restore-snapshot-btn'));
    const restoreSnapshotButton = await screen.findByRole('button', { name: 'Restore' });

    vi.mocked(window.confirm).mockReturnValueOnce(false);
    await fireEvent.click(restoreSnapshotButton);
    expect(invokeMock.mock.calls.some(([command]) => command === 'restore_db_from_snapshot')).toBe(
      false,
    );

    invokeMock.mockImplementation((command, args) => {
      if (command === 'restore_db_from_snapshot') {
        return Promise.reject(new Error('snapshot refused'));
      }
      if (command === 'db_restore') return Promise.reject(new Error('file refused'));
      return successfulInvoke(command, args);
    });
    await fireEvent.click(restoreSnapshotButton);
    await waitFor(() => expect(restoreSnapshotButton).not.toBeDisabled());
    expect(get(notifications).at(-1)).toMatchObject({ type: 'error' });

    chooseFileMock.mockResolvedValueOnce(null);
    await fireEvent.click(screen.getByTestId('restore-file-btn'));
    expect(invokeMock.mock.calls.some(([command]) => command === 'db_restore')).toBe(false);

    chooseFileMock.mockRejectedValueOnce(new Error('picker refused'));
    await fireEvent.click(screen.getByTestId('restore-file-btn'));
    await waitFor(() => expect(get(notifications).at(-1)?.type).toBe('error'));

    vi.mocked(window.confirm).mockReturnValueOnce(false);
    await fireEvent.click(screen.getByTestId('restore-file-btn'));
    expect(invokeMock.mock.calls.some(([command]) => command === 'db_restore')).toBe(false);

    await fireEvent.click(screen.getByTestId('restore-file-btn'));
    await waitFor(() =>
      expect(invokeMock.mock.calls.some(([command]) => command === 'db_restore')).toBe(true),
    );
    await waitFor(() => expect(screen.getByTestId('restore-file-btn')).not.toBeDisabled());
  });

  it('hands a verified file restore to the desktop reload boundary and executes the debounced refresh', async () => {
    await renderDesktop();

    await fireEvent.click(screen.getByTestId('restore-file-btn'));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith('db_restore', {
        src: 'C:\\owner-backups\\backup.db',
      }),
    );
    expect(reloadAppMock).toHaveBeenCalledOnce();

    await waitFor(
      () => {
        expect(
          invokeMock.mock.calls.filter(([command]) => command === 'get_dataset_stats').length,
        ).toBeGreaterThanOrEqual(2);
        expect(
          invokeMock.mock.calls.filter(([command]) => command === 'get_inference_stats').length,
        ).toBeGreaterThanOrEqual(2);
      },
      { timeout: 1_500 },
    );
  });

  it('keeps optional evidence hidden when probes fail and never promotes unknown readiness to green', async () => {
    invokeMock.mockImplementation((command, args) => {
      if (
        [
          'get_fingerprint_count',
          'get_dataset_certificate',
          'get_audio_health',
          'get_training_grade_breakdown',
          'list_eval_runs',
          'get_inference_stats',
          'app_git_sha',
          'get_intelligence_report',
        ].includes(command)
      ) {
        return Promise.reject(new Error(`${command} unavailable`));
      }
      return successfulInvoke(command, args);
    });

    render(StatsDashboard);
    expect(await screen.findByTestId('readiness-headline')).toHaveTextContent('Readiness unknown');
    expect(screen.queryByTestId('accuracy-record')).not.toBeInTheDocument();
    expect(screen.queryByTestId('accuracy-none')).not.toBeInTheDocument();
    expect(screen.queryByTestId('stat-fingerprints')).not.toBeInTheDocument();
    expect(screen.queryByTestId('build-sha')).not.toBeInTheDocument();
    expect(screen.queryByTestId('intelligence-report')).not.toBeInTheDocument();
    expect(get(notifications).some((item) => item.type === 'error')).toBe(false);
  });

  it('separates a primary load failure from an honestly empty response', async () => {
    invokeMock.mockImplementation((command, args) => {
      if (command === 'get_dataset_stats') {
        return Promise.reject({
          schema: 1,
          code: 'STATS_READ_FAILED',
          message: 'private database path',
          retryable: true,
          operationId: 'stats-op',
        });
      }
      return successfulInvoke(command, args);
    });
    const failed = render(StatsDashboard);
    expect(await screen.findByText('Failed to load stats')).toBeInTheDocument();
    expect(screen.getByText(/STATS_READ_FAILED/)).toBeInTheDocument();
    expect(screen.queryByText('private database path')).not.toBeInTheDocument();
    expect(get(notifications).at(-1)).toMatchObject({ type: 'error' });
    failed.unmount();
    notifications.clear();

    invokeMock.mockImplementation((command, args) => {
      if (command === 'get_dataset_stats' || command === 'get_dataset_quality') {
        return Promise.resolve(null);
      }
      return successfulInvoke(command, args);
    });
    render(StatsDashboard);
    expect(await screen.findByText('No data available')).toBeInTheDocument();
    expect(screen.getByText(/Load segments to see dataset statistics/)).toBeInTheDocument();
    expect(get(notifications).some((item) => item.type === 'error')).toBe(false);
  });
});
