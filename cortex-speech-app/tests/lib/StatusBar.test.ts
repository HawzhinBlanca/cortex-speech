import { fireEvent, render, screen } from '@testing-library/svelte';
import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import StatusBar from '../../src/lib/StatusBar.svelte';
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
  statusMessage,
} from '../../src/lib/stores/uiStore';

const invokeMock = vi.mocked(invoke);

describe('StatusBar', () => {
  beforeEach(() => {
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
});
