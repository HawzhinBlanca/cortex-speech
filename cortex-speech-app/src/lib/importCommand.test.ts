import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, expectTypeOf, it, vi } from 'vitest';
import {
  discardInterruptedImport,
  getInterruptedImport,
  resumeInterruptedImport,
  type ImportJob,
} from './commands';
import type { CommandErrorV1, ImportJobV1, ImportResumeV1 } from './generated/ipc';

const invokeMock = vi.mocked(invoke);

describe('generated interrupted-import command boundary', () => {
  const runId = '00000000-0000-4000-8000-000000000001';
  beforeEach(() => invokeMock.mockReset());

  it('uses exact command arguments and returns only the renderer-safe progress DTO', async () => {
    const job: ImportJobV1 = {
      id: 'import-job-1',
      totalFiles: 19,
      completedCount: 7,
      createdAt: '2026-08-28T10:00:00Z',
    };
    const resumed: ImportResumeV1 = {
      status: 'started',
      resuming: true,
      importJobId: 'import-job-2',
      runId,
    };
    invokeMock
      .mockResolvedValueOnce(job)
      .mockResolvedValueOnce(resumed)
      .mockResolvedValueOnce(null);

    await expect(getInterruptedImport()).resolves.toEqual(job);
    await expect(resumeInterruptedImport('import-job-1', runId)).resolves.toEqual(resumed);
    await expect(discardInterruptedImport('import-job-2')).resolves.toBeUndefined();

    expect(invokeMock.mock.calls).toEqual([
      ['get_interrupted_import'],
      ['resume_interrupted_import', { jobId: 'import-job-1', runId }],
      ['discard_interrupted_import', { jobId: 'import-job-2' }],
    ]);
    expectTypeOf<ImportJob>().toEqualTypeOf<ImportJobV1>();
    expect(job).not.toHaveProperty('dir');
    expect(job).not.toHaveProperty('completedPaths');
  });

  it('preserves a structured backend refusal without stringifying it', async () => {
    const refusal: CommandErrorV1 = {
      schema: 1,
      code: 'IMPORT_SOURCE_MISSING',
      message: 'The interrupted import folder is no longer available.',
      retryable: false,
      suggestedAction: null,
      operationId: null,
      details: {},
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(resumeInterruptedImport('import-job-1', runId)).rejects.toEqual(refusal);
  });
});
