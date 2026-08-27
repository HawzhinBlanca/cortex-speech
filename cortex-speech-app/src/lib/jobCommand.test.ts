import { invoke } from '@tauri-apps/api/core';
import { describe, expect, expectTypeOf, it, vi } from 'vitest';
import { getJobs, type Job } from './commands';
import type { CommandErrorV1, JobV1 } from './generated/ipc';

const invokeMock = vi.mocked(invoke);

describe('generated Job Center IPC contract', () => {
  it('returns the exact durable job wire shape', async () => {
    invokeMock.mockReset();
    const jobs: JobV1[] = [
      {
        id: 'job-1',
        kind: 'import',
        state: 'running',
        progress: 0.5,
        completed: 1,
        total: 2,
        errorCode: null,
      },
    ];
    invokeMock.mockResolvedValueOnce(jobs);

    await expect(getJobs()).resolves.toEqual(jobs);
    expect(invokeMock).toHaveBeenCalledWith('get_jobs');
    expectTypeOf<Job>().toEqualTypeOf<JobV1>();
  });

  it('preserves the structured recovery error', async () => {
    invokeMock.mockReset();
    const refusal: CommandErrorV1 = {
      schema: 1,
      code: 'JOB_CENTER_UNAVAILABLE',
      message: 'The Job Center could not read durable operation status.',
      retryable: true,
      suggestedAction: 'openHealth',
      operationId: null,
      details: {},
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(getJobs()).rejects.toEqual(refusal);
  });
});
