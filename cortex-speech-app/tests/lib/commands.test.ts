import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import {
  AudioExportFormat,
  ASR_7B_UNAVAILABLE_TAG,
  is7bUnavailableError,
  listAgentImportReports,
  listAgentStageEvents,
  recordHumanDecision,
  recordReviewFlag,
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

describe('desktop review decision idempotency', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('replays one uncertain invoke with the exact same operation identity and payload', async () => {
    const commit = {
      effectEventId: 41,
      segmentId: 'segment-1',
      effectiveAction: 'edit',
      priorRevision: 3,
      decidedRevision: 4,
      segment: { id: 'segment-1' },
    };
    invokeMock
      .mockRejectedValueOnce(new Error('transport response lost'))
      .mockResolvedValueOnce(commit);

    await expect(recordHumanDecision('segment-1', 'edit', 'دەقی ڕاست', 1_777_000)).resolves.toBe(
      commit,
    );

    expect(invokeMock).toHaveBeenCalledTimes(2);
    const first = invokeMock.mock.calls[0];
    const second = invokeMock.mock.calls[1];
    expect(first[0]).toBe('record_human_decision');
    expect(second[0]).toBe('record_human_decision');
    expect(second[1]).toEqual(first[1]);
    expect(first[1]).toMatchObject({
      segmentId: 'segment-1',
      decision: 'edit',
      correctedTranscript: 'دەقی ڕاست',
      timestampMs: 1_777_000,
      operationId: expect.stringMatching(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
      ),
    });
  });
});

describe('desktop review flag idempotency', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('replays one uncertain invoke with the exact same flag operation identity and payload', async () => {
    const commit = {
      effectEventId: 52,
      segmentId: 'segment-2',
      priorRevision: 7,
      flagRevision: 8,
      segment: { id: 'segment-2' },
    };
    invokeMock
      .mockRejectedValueOnce(new Error('transport response lost'))
      .mockResolvedValueOnce(commit);

    await expect(recordReviewFlag('segment-2', 'needs a second listen')).resolves.toBe(commit);

    expect(invokeMock).toHaveBeenCalledTimes(2);
    const first = invokeMock.mock.calls[0];
    const second = invokeMock.mock.calls[1];
    expect(first[0]).toBe('record_review_flag');
    expect(second[0]).toBe('record_review_flag');
    expect(second[1]).toEqual(first[1]);
    expect(first[1]).toMatchObject({
      segmentId: 'segment-2',
      rationale: 'needs a second listen',
      operationId: expect.stringMatching(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
      ),
    });
  });
});
