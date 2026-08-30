import { describe, expect, it } from 'vitest';
import { locale } from './i18n';
import {
  publicAgentStagePresentation,
  publicBatchHaltDetail,
  publicPipelineErrorPresentation,
  publicPipelineProgressPresentation,
} from './events';

describe('publicPipelineProgressPresentation', () => {
  it('strips spoofing controls and maps only closed status codes to localized copy', () => {
    locale.set('en');
    const result = publicPipelineProgressPresentation({
      runId: 'run-1',
      current: 2.9,
      total: Number.POSITIVE_INFINITY,
      fileLabel: 'D:\\private\\safe\u202egnp.exe.wav',
      status: 'transcribing',
      detail: 'token=secret SELECT * FROM segments',
    });

    expect(result).toEqual({
      runId: 'run-1',
      current: 0,
      total: 0,
      file: 'safegnp.exe.wav',
      phase: 'transcribing',
      status: 'Transcribing...',
    });
    expect(JSON.stringify(result)).not.toMatch(/private|secret|SELECT|202e/);
  });

  it('is total for hostile accessors and rejects free-form statuses', () => {
    locale.set('en');
    const hostile = Object.defineProperty({}, 'status', {
      get: () => {
        throw new Error('D:\\private\\getter');
      },
    });
    expect(() => publicPipelineProgressPresentation(hostile)).not.toThrow();
    expect(publicPipelineProgressPresentation(hostile)).toEqual({
      runId: null,
      current: 0,
      total: 0,
      file: 'unknown file',
      phase: 'importing',
      status: 'Importing...',
    });
    expect(
      publicPipelineProgressPresentation({
        runId: 'run-1',
        fileLabel: 'safe.wav',
        status: 'D:\\private\\backend prose',
      }).status,
    ).toBe('Importing...');
  });
});

describe('publicPipelineErrorPresentation', () => {
  it('keeps only a bounded basename and maps closed error codes to localized copy', () => {
    locale.set('en');
    const result = publicPipelineErrorPresentation({
      runId: 'run-1',
      file: 'D:\\private\\Wareen\\source\u0000.wav',
      code: 'IMPORT_ENRICHMENT_FAILED',
      error: 'secret-token SELECT * FROM import_jobs',
    });

    expect(result.runId).toBe('run-1');
    expect(result.file).toBe('source.wav');
    expect(result.detail).toContain('import is saved');
    expect(JSON.stringify(result)).not.toContain('Wareen');
    expect(JSON.stringify(result)).not.toContain('secret-token');
    expect(JSON.stringify(result)).not.toContain('SELECT');
  });

  it('is total for malformed and accessor-hostile event payloads', () => {
    locale.set('en');
    const hostile = Object.defineProperty({}, 'file', {
      get: () => {
        throw new Error('private getter failure');
      },
    });

    expect(() => publicPipelineErrorPresentation(hostile)).not.toThrow();
    expect(publicPipelineErrorPresentation(hostile)).toEqual({
      runId: null,
      file: 'unknown file',
      detail: 'Processing stopped safely. Retry; open Health if it continues.',
    });
    expect(publicPipelineErrorPresentation(null).file).toBe('unknown file');
  });
});

describe('publicAgentStagePresentation', () => {
  it('ignores native detail, strips path ancestry, and derives a closed localized status', () => {
    locale.set('en');
    const result = publicAgentStagePresentation({
      runId: 'run-1',
      stage: 'jury_adjudication',
      status: 'blocked',
      fileLabel: 'D:\\private\\owner\\meet\u202eing.wav',
      detail: 'token=secret SELECT * FROM segments',
      detailCode: 'HOSTILE_UNKNOWN_CODE',
      current: 7,
      total: 9,
    });

    expect(result).toEqual({
      runId: 'run-1',
      stage: 'jury_adjudication',
      status: 'blocked',
      file: 'meeting.wav',
      detail: 'Blocked',
      current: 7,
      total: 9,
    });
    expect(JSON.stringify(result)).not.toMatch(/private|secret|SELECT|HOSTILE/);
  });

  it('accepts the legitimate champion-only not-required hypothesis stage', () => {
    locale.set('en');
    const result = publicAgentStagePresentation({
      runId: 'run-1',
      stage: 'multi_model_hypotheses',
      status: 'not_required',
      fileLabel: 'clip.wav',
      detail: 'D:\\private\\owner\\should-not-render',
      detailCode: 'NOT_REQUIRED',
      current: 0,
      total: 0,
    });

    expect(result).toEqual({
      runId: 'run-1',
      stage: 'multi_model_hypotheses',
      status: 'not_required',
      file: 'clip.wav',
      detail: 'Not required',
      current: 0,
      total: 0,
    });
    expect(JSON.stringify(result)).not.toMatch(/private|owner|should-not-render|NOT_REQUIRED/);
  });

  it('rejects unknown vocabulary and is total for hostile accessors and counts', () => {
    locale.set('en');
    expect(
      publicAgentStagePresentation({ runId: 'run-1', stage: 'D:\\private', status: 'completed' }),
    ).toBeNull();
    expect(
      publicAgentStagePresentation({ runId: 'run-1', stage: 'agent_report', status: 'invented' }),
    ).toBeNull();
    expect(
      publicAgentStagePresentation({ runId: 'run-1', stage: 'agent_report', status: '__proto__' }),
    ).toBeNull();
    expect(
      publicAgentStagePresentation({
        runId: 'run-1',
        stage: 'agent_report',
        status: 'completed',
        file: '',
        current: Number.POSITIVE_INFINITY,
        total: -1,
      }),
    ).toMatchObject({ file: 'unknown file', current: 0, total: 0 });

    const hostile = Object.defineProperty({}, 'stage', {
      get: () => {
        throw new Error('D:\\private\\getter');
      },
    });
    expect(() => publicAgentStagePresentation(hostile)).not.toThrow();
    expect(publicAgentStagePresentation(hostile)).toBeNull();
  });
});

describe('publicBatchHaltDetail', () => {
  it('maps closed halt codes to localized copy and never renders native detail', () => {
    locale.set('en');
    const error = {
      schema: 1,
      code: 'BATCH_TRANSCRIPT_WRITE_FAILED',
      message: 'D:\\private\\owner token=secret SELECT * FROM speech_segments',
      retryable: true,
    };
    const detail = publicBatchHaltDetail(error);
    expect(detail).toContain('could not be saved durably');
    expect(detail).not.toMatch(/private|owner|secret|SELECT/);
    expect(publicBatchHaltDetail({ code: 'PROCESS_INTERRUPTED' })).toContain(
      'previous desktop process interrupted',
    );
  });

  it('uses a closed generic message for malformed and accessor-hostile errors', () => {
    locale.set('en');
    const hostile = Object.defineProperty({}, 'code', {
      get: () => {
        throw new Error('private backend failure');
      },
    });
    expect(() => publicBatchHaltDetail(hostile)).not.toThrow();
    expect(publicBatchHaltDetail(hostile)).toContain('stopped safely');
    expect(publicBatchHaltDetail({ code: 'HOSTILE_UNKNOWN_CODE' })).toContain('stopped safely');
  });
});
