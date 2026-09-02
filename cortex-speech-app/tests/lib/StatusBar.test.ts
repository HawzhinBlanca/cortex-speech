import { fireEvent, render, screen } from '@testing-library/svelte';
import { invoke } from '@tauri-apps/api/core';
import { get } from 'svelte/store';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import StatusBar from '../../src/lib/StatusBar.svelte';
import { locale } from '../../src/lib/i18n';
import { segmentStats, segments } from '../../src/lib/stores/segmentStore';
import {
  agentPipelineStages,
  batchProgress,
  filesProcessed,
  isProcessing,
  pipelineCurrentFile,
  pipelinePhase,
  pipelineStatus,
  pipelineTotal,
  showKeyboardHelp,
  statusMessage,
} from '../../src/lib/stores/uiStore';

const invokeMock = vi.mocked(invoke);

describe('StatusBar', () => {
  beforeEach(() => {
    locale.set('en');
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined);
    segments.set([]);
    segmentStats.set({ total: 0, verified: 0, pending: 0, withAnnotations: 0, totalDurationMs: 0 });
    statusMessage.set('Ready');
    isProcessing.set(false);
    pipelinePhase.set('idle');
    pipelineCurrentFile.set('');
    pipelineStatus.set('');
    pipelineTotal.set(0);
    filesProcessed.set(0);
    batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
    agentPipelineStages.set([]);
    showKeyboardHelp.set(false);
  });

  it('keeps the status summary visible after extraction from the workstation', () => {
    render(StatusBar);
    expect(screen.getByTestId('status-bar')).toHaveTextContent('Ready');
  });

  it('keeps batch cancellation wired to the typed command service', async () => {
    pipelinePhase.set('transcribing');
    pipelineStatus.set('Drafting');
    pipelineTotal.set(4);
    filesProcessed.set(2);
    render(StatusBar);

    await fireEvent.click(screen.getByTestId('cancel-batch-transcribe-btn'));
    expect(invokeMock).toHaveBeenCalledWith('cancel_operation');
  });

  it('renders the persisted agent pipeline timeline', () => {
    agentPipelineStages.set([
      {
        runId: 'run-1',
        stage: 'dataset_promotion',
        status: 'blocked',
        file: 'run',
        detail: 'human review required',
        current: 1,
        total: 1,
        updatedAt: Date.UTC(2026, 7, 26),
      },
    ]);
    render(StatusBar);

    expect(screen.getByTestId('agent-pipeline-timeline')).toHaveTextContent(
      'dataset promotion:blocked',
    );
  });

  it('renders importing identity, bounded basename, optional phase detail, and cancellation', async () => {
    isProcessing.set(true);
    pipelinePhase.set('importing');
    pipelineCurrentFile.set('C:\\private\\owner\\clip.wav');
    pipelineStatus.set('Reading audio');
    pipelineTotal.set(0);
    filesProcessed.set(2);
    render(StatusBar);

    expect(screen.getByText('Processing')).toBeInTheDocument();
    expect(screen.getByTestId('pipeline-import-status')).toHaveTextContent('2/? files');
    expect(screen.getByTestId('pipeline-import-status')).toHaveTextContent('clip.wav');
    expect(screen.getByTestId('pipeline-import-status')).not.toHaveTextContent('private');
    expect(screen.getByTestId('pipeline-import-status')).toHaveTextContent('Reading audio');
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(invokeMock).toHaveBeenCalledWith('cancel_operation');
  });

  it('renders every long-running phase and all transcribing progress fallbacks', async () => {
    render(StatusBar);

    pipelinePhase.set('reference_transcribing');
    pipelineCurrentFile.set('D:/audio/reference.wav');
    pipelineStatus.set('Champion pass');
    await vi.waitFor(() =>
      expect(screen.getByTestId('pipeline-reference-status')).toHaveTextContent('reference.wav'),
    );
    expect(screen.getByTestId('pipeline-reference-status')).toHaveTextContent('Champion pass');

    pipelinePhase.set('detecting');
    await vi.waitFor(() =>
      expect(screen.getByTestId('status-bar')).toHaveTextContent('Detecting speech'),
    );

    pipelinePhase.set('transcribing');
    pipelineStatus.set('');
    filesProcessed.set(0);
    pipelineTotal.set(0);
    batchProgress.set({ status: 'running', completed: 3, total: 5, percent: 60 });
    await vi.waitFor(() =>
      expect(screen.getByTestId('status-bar')).toHaveTextContent('Transcribing'),
    );
    expect(screen.getByTestId('status-bar')).toHaveTextContent('3/5');

    batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
    await vi.waitFor(() => expect(screen.getByTestId('status-bar')).toHaveTextContent('0/?'));

    pipelinePhase.set('adjudicating');
    await vi.waitFor(() =>
      expect(screen.getByTestId('status-bar')).toHaveTextContent('Adjudicating'),
    );
  });

  it('renders completed, blocked, and running stage tones while retaining only the latest five', () => {
    agentPipelineStages.set(
      [
        ['old_stage', 'completed'],
        ['reference_transcript', 'completed'],
        ['jury_gate', 'blocked'],
        ['cloud_advisory', 'running'],
        ['dataset_promotion', 'needs_review'],
        ['release_gate', 'completed'],
      ].map(([stage, status], index) => ({
        runId: 'run-1',
        stage,
        status,
        file: 'run',
        detail: `detail ${index}`,
        current: index,
        total: 6,
        updatedAt: index,
      })),
    );
    render(StatusBar);
    const timeline = screen.getByTestId('agent-pipeline-timeline');
    expect(timeline).not.toHaveTextContent('old stage');
    expect(timeline).toHaveTextContent('reference transcript:completed');
    expect(timeline).toHaveTextContent('jury gate:blocked');
    expect(timeline).toHaveTextContent('cloud advisory:running');
    expect(timeline.querySelector('.border-emerald-700\\/40')).toBeTruthy();
    expect(timeline.querySelector('.border-red-700\\/40')).toBeTruthy();
    expect(timeline.querySelector('.border-amber-700\\/40')).toBeTruthy();
  });

  it('shows idle batch progress, exact corpus duration, and opens keyboard help', async () => {
    segmentStats.set({
      total: 12,
      verified: 8,
      pending: 4,
      withAnnotations: 0,
      totalDurationMs: 125_000,
    });
    pipelinePhase.set('idle');
    batchProgress.set({ status: 'running', completed: 2, total: 4, percent: 50 });
    render(StatusBar);

    expect(screen.getByTestId('status-bar')).toHaveTextContent('2/4');
    expect(screen.getByTestId('status-bar')).toHaveTextContent('2:05 total');
    expect(screen.getByTestId('status-bar')).toHaveTextContent('8/12 verified');
    await fireEvent.click(screen.getByRole('button', { name: /keyboard shortcuts/i }));
    expect(get(showKeyboardHelp)).toBe(true);
  });
});
