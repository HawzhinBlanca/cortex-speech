import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import ValidationPanel from '../../src/lib/ValidationPanel.svelte';
import { ckb } from '../../src/lib/i18n/ckb';

const mocks = vi.hoisted(() => ({
  validateDataset: vi.fn(),
  getActiveLearningQueue: vi.fn(),
  getSignalAnomalySegments: vi.fn(),
  computeSignalAnomalyScores: vi.fn(),
}));

vi.mock('../../src/lib/commands', () => mocks);

describe('ValidationPanel bounded data sources', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.validateDataset.mockResolvedValue({
      summary: 'Checked 2 clips',
      totalSegments: 2,
      passed: 1,
      errors: [{ category: 'audio', message: 'missing', segmentId: 's1', severity: 'Error' }],
      warnings: [],
    });
    mocks.getActiveLearningQueue.mockResolvedValue([]);
    mocks.getSignalAnomalySegments.mockResolvedValue([]);
    mocks.computeSignalAnomalyScores.mockResolvedValue(2);
  });

  it('validates on mount and uses bounded endpoints for secondary tabs', async () => {
    render(ValidationPanel);
    expect(await screen.findByText('Checked 2 clips')).toBeInTheDocument();
    expect(mocks.validateDataset).toHaveBeenCalledTimes(1);

    await fireEvent.click(screen.getByRole('button', { name: ckb['validation.tab.activeLearning'] }));
    await waitFor(() => expect(mocks.getActiveLearningQueue).toHaveBeenCalledWith(0.05, 0.95, 20));

    await fireEvent.click(screen.getByRole('button', { name: ckb['validation.tab.signalAnomaly'] }));
    await waitFor(() => expect(mocks.getSignalAnomalySegments).toHaveBeenCalledWith(100));
    expect(mocks.getSignalAnomalySegments).toHaveBeenCalledTimes(1);
  });
});
