import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import RefineryPanel from '../../src/lib/RefineryPanel.svelte';

const invokeMock = vi.mocked(invoke);

describe('RefineryPanel browser runtime', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  afterEach(() => {
    cleanup();
  });

  it('surfaces eval-run history and escalation trend from IPC (M3.3)', async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === 'list_eval_runs') {
        return [
          { id: 'run-1', modelId: 'omniasr-ctc-300m', runAt: '2026-06-23', numSegs: 12, wer: 0.1, cer: 0.05 },
        ];
      }
      if (cmd === 'get_escalation_rate_trend') {
        return [{ date: '2026-06-23', escalationRate: 0.25, total: 8, escalated: 2 }];
      }
      return [];
    });

    render(RefineryPanel);

    // Wait for the async IPC load to resolve (replaces the loading state), then assert the
    // panel that previously rendered in ZERO components now shows real refinery data.
    expect(await screen.findByText('omniasr-ctc-300m')).toBeInTheDocument();
    expect(screen.getByTestId('refinery-panel')).toBeInTheDocument();
    expect(screen.getByTestId('refinery-eval-runs')).toBeInTheDocument();
    expect(screen.getByTestId('refinery-escalation-trend')).toBeInTheDocument();
    expect(screen.getByText('10.0%')).toBeInTheDocument(); // WER
    expect(screen.getByText('5.0%')).toBeInTheDocument(); // CER
    expect(screen.getByText('25.0% (2/8)')).toBeInTheDocument(); // escalation rate

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('list_eval_runs');
      expect(invokeMock).toHaveBeenCalledWith('get_escalation_rate_trend');
    });
  });
});
