import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import {
  AudioExportFormat,
  listAgentImportReports,
  listAgentStageEvents,
} from '../../src/lib/commands';

const invokeMock = vi.mocked(invoke);

describe('commands audio export contract', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('only advertises formats the backend can actually encode', () => {
    expect(AudioExportFormat).toEqual({ Wav: 'Wav' });
    expect(Object.values(AudioExportFormat)).not.toContain('Flac');
  });

  it('lists agent import reports through the registered backend command', async () => {
    invokeMock.mockResolvedValueOnce([]);

    await expect(listAgentImportReports(7)).resolves.toEqual([]);

    expect(invokeMock).toHaveBeenCalledWith('list_agent_import_reports', { limit: 7 });
  });

  it('lists persisted agent stage events through the registered backend command', async () => {
    invokeMock.mockResolvedValueOnce([]);

    await expect(listAgentStageEvents('run-1', 9)).resolves.toEqual([]);

    expect(invokeMock).toHaveBeenCalledWith('list_agent_stage_events', {
      runId: 'run-1',
      limit: 9,
    });
  });
});
