import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import {
  AudioExportFormat,
  ASR_7B_UNAVAILABLE_TAG,
  is7bUnavailableError,
  listAgentImportReports,
  listAgentStageEvents,
} from '../../src/lib/commands';

const invokeMock = vi.mocked(invoke);

describe('7B-champion-unavailable detection (never silently downgrade)', () => {
  it('matches the sentinel whether the error is a bare string or an Error object', () => {
    // Tauri rejects invoke() with the backend error STRING; some paths wrap it in an Error.
    expect(is7bUnavailableError(`${ASR_7B_UNAVAILABLE_TAG}: server not responding`)).toBe(true);
    expect(is7bUnavailableError(new Error(`${ASR_7B_UNAVAILABLE_TAG}: timed out`))).toBe(true);
  });

  it('does NOT match ordinary transcription errors (those show the normal error, not the choice)', () => {
    expect(is7bUnavailableError('Empty audio file')).toBe(false);
    expect(is7bUnavailableError(new Error('ONNX inference failed'))).toBe(false);
    expect(is7bUnavailableError(null)).toBe(false);
    expect(is7bUnavailableError(undefined)).toBe(false);
  });
});

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
