import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import App from '../../src/App.svelte';
import { locale } from '../../src/lib/i18n';

const invokeMock = vi.mocked(invoke);

function segmentFixture() {
  return {
    id: 'seg-1',
    audioPath: 'C:\\audio\\long.wav',
    rawTranscript: 'candidate text',
    normalizedTranscript: null,
    annotatedTranscript: null,
    alignmentJson: null,
    durationMs: 2000,
    speakerId: null,
    verified: false,
  };
}

function backendSettingsFixture() {
  return {
    model_dir: 'models',
    output_dir: 'exports',
    asr_model_size: 'CTC300M',
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
    max_speakers: 8,
    max_wer_threshold: 0.35,
    max_cer_threshold: 0.2,
    enforce_quality_gates: false,
    theme: 'Dark',
    llm_mode: 'Local',
    llm_endpoint: 'http://127.0.0.1:11434/v1/chat/completions',
    llm_api_key: '',
    llm_api_key_configured: false,
    cloud_llm_opt_in: false,
    llm_system_prompt: '',
    llm_model: 'heretic-final:latest',
    external_asr_script_path: '',
    hf_train_ratio: 0.8,
    hf_val_ratio: 0.1,
    hf_test_ratio: 0.1,
    hf_split_seed: 42,
    hf_speaker_disjoint: true,
    hf_license: 'mit',
  };
}

function blockedAgentReportFixture() {
  return {
    id: 'report-1',
    agentRunId: 'run-blocked-1',
    source: 'file',
    status: 'completed',
    summary: {
      totalSegments: 1,
      agenticReadiness: null,
      sourceReferences: [],
      sourceReferenceRequired: false,
      requiredSourceReferenceModels: [],
      sourceReferenceModels: [],
      sourceReferenceCoverage: [],
      longFileDossiers: [],
      hypothesisModels: ['omniasr-wsl-7b'],
      hypothesisModelCounts: { 'omniasr-wsl-7b': 1 },
      verdictCounts: { jury_accept: 1 },
      escalatedSegments: [],
      trainingGradeSummary: {
        totalSegments: 1,
        trainingReadySegments: 1,
        goldSegments: 0,
        silverSegments: 1,
        reviewSegments: 0,
        rejectedSegments: 0,
      },
      trainingGradeReasonCounts: {},
      hypothesisCoverageBlockers: [],
      orchestrationStages: [
        {
          stage: 'dataset_promotion',
          status: 'blocked',
          detailCode: 'blocked',
          blockerCount: 1,
        },
      ],
    },
    errorCode: null,
    createdAt: '2026-06-16T12:00:00Z',
  };
}

function datasetStatsFixture() {
  return {
    totalSegments: 1,
    totalDurationSeconds: 2,
    avgDurationSeconds: 2,
    verifiedCount: 0,
    pendingCount: 1,
    verificationRate: 0,
    uniqueSpeakers: 1,
    totalChars: 14,
    avgCharsPerSegment: 14,
    durationHistogram: { under5s: 1, under10s: 0, under15s: 0, under30s: 0, over30s: 0 },
    topSpeakers: [{ speakerId: 'unknown', segmentCount: 1, totalDurationSeconds: 2 }],
  };
}

describe('App desktop runtime guard', () => {
  async function openOverflowMenu(): Promise<void> {
    await fireEvent.click(await screen.findByTestId('header-overflow-btn'));
    await screen.findByTestId('header-overflow-menu');
  }

  beforeEach(() => {
    invokeMock.mockReset();
    delete window.__TAURI__;
    delete window.__TAURI_INTERNALS__;
    localStorage.clear();
    locale.set('en');
    vi.mocked(window.matchMedia).mockImplementation((query: string) => ({
      matches: query.includes('min-width'),
      media: query,
      onchange: null,
      addListener: vi.fn(),
      removeListener: vi.fn(),
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      dispatchEvent: vi.fn(),
    }));
  });

  afterEach(() => {
    cleanup();
  });

  it('starts in browser mode without invoking Tauri and disables desktop-only actions', async () => {
    render(App);
    await openOverflowMenu();

    const openButton = await screen.findByRole('button', { name: 'Open audio file' });

    await waitFor(() => expect(invokeMock).not.toHaveBeenCalled());
    expect(openButton).toBeDisabled();
    expect(openButton).toHaveAttribute('title', 'Desktop app runtime required');

    expect(screen.getByRole('button', { name: 'Import directory' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Speakers' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Merge' })).toBeDisabled();
  });

  it('blocks HF training export when the latest dataset promotion stage is blocked', async () => {
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: 'main' } },
      transformCallback: vi.fn(() => 1),
    };
    invokeMock.mockImplementation((command: string) => {
      if (command === 'get_segments_page') {
        return Promise.resolve({ items: [segmentFixture()], total: 1, nextCursor: null });
      }
      if (command === 'list_agent_import_reports')
        return Promise.resolve([blockedAgentReportFixture()]);
      if (command === 'list_agent_stage_events') return Promise.resolve([]);
      if (command === 'get_settings') return Promise.resolve(backendSettingsFixture());
      if (command === 'get_dataset_stats') return Promise.resolve(datasetStatsFixture());
      if (command === 'get_dataset_quality') {
        return Promise.resolve({
          totalSegments: 1,
          emptyTranscriptCount: 0,
          lowConfidenceCount: 0,
          duplicateTranscriptGroups: 0,
          duplicateTranscriptSegments: 0,
          durationOutlierCount: 0,
          medianDurationMs: 2000,
          q1DurationMs: 2000,
          q3DurationMs: 2000,
          duplicateGroups: [],
          durationOutliers: [],
          annotatedSegmentCount: 0,
          meanWer: null,
          meanCer: null,
          segmentsAboveWerThreshold: 0,
          segmentsAboveCerThreshold: 0,
          qualityGatePassed: true,
          werOutliers: [],
        });
      }
      if (command === 'get_dataset_certificate') return Promise.resolve(null);
      if (command === 'get_inference_stats') return Promise.resolve(null);
      return Promise.resolve(null);
    });

    render(App);
    await openOverflowMenu();

    const hfExport = await screen.findByTestId('hf-export-btn');

    await waitFor(() =>
      expect(hfExport).toHaveAttribute('title', expect.stringContaining('1 blocker(s)')),
    );
    expect(hfExport.getAttribute('title')).not.toContain(
      'training-ready machine segment still lacks multi-model coverage',
    );
    expect(invokeMock).toHaveBeenCalledWith('list_agent_stage_events', {
      runId: 'run-blocked-1',
      limit: 25,
    });
    expect(hfExport).toBeDisabled();
    expect(invokeMock).not.toHaveBeenCalledWith('export_huggingface_dataset', expect.anything());
  });

  it('opens settings in browser mode without persisting through Tauri', async () => {
    render(App);
    await openOverflowMenu();

    await fireEvent.click(await screen.findByRole('button', { name: 'Open settings' }));
    expect(await screen.findByTestId('settings-panel')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'AI Models' })).toBeDisabled();

    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(screen.queryByTestId('settings-panel')).not.toBeInTheDocument());
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
