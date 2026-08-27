import { invoke } from '@tauri-apps/api/core';
import { describe, expect, it, vi } from 'vitest';
import {
  getDatasetCertificate,
  getDatasetQuality,
  getDatasetStats,
  getLabelQualityLift,
  getTrainingGradeBreakdown,
} from './commands';

const invokeMock = vi.mocked(invoke);

describe('dataset analytics IPC contract', () => {
  it('routes every analytics read through its generated command and preserves arguments', async () => {
    invokeMock.mockReset();
    const stats = { totalSegments: 12 };
    const quality = { totalSegments: 12, qualityGatePassed: true };
    const grade = {
      summary: {
        totalSegments: 12,
        trainingReadySegments: 10,
        goldSegments: 8,
        silverSegments: 2,
        reviewSegments: 2,
        rejectedSegments: 0,
      },
      reasonCounts: {},
    };
    const certificate = { targetError: 0.05, confidenceLevel: 0.95 };
    const lift = { n: 12, cerLift: 0.1 };
    invokeMock
      .mockResolvedValueOnce(stats)
      .mockResolvedValueOnce(quality)
      .mockResolvedValueOnce(grade)
      .mockResolvedValueOnce(certificate)
      .mockResolvedValueOnce(lift);

    await expect(getDatasetStats()).resolves.toBe(stats);
    await expect(getDatasetQuality()).resolves.toBe(quality);
    await expect(getTrainingGradeBreakdown()).resolves.toBe(grade);
    await expect(getDatasetCertificate(0.05, 0.95)).resolves.toBe(certificate);
    await expect(getLabelQualityLift()).resolves.toBe(lift);

    expect(invokeMock.mock.calls).toEqual([
      ['get_dataset_stats'],
      ['get_dataset_quality'],
      ['get_training_grade_breakdown'],
      ['get_dataset_certificate', { targetError: 0.05, confidenceLevel: 0.95 }],
      ['get_label_quality_lift'],
    ]);
  });

  it('propagates a structured analytics refusal', async () => {
    invokeMock.mockReset();
    const refusal = {
      schema: 1,
      code: 'DATASET_STATS_FAILED',
      message: 'The dataset summary could not be computed. Open Health for recovery options.',
      retryable: false,
      suggestedAction: 'openHealth',
      operationId: null,
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(getDatasetStats()).rejects.toEqual(refusal);
  });
});
