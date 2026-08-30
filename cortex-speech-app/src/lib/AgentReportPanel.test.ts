import { cleanup, render } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import AgentReportPanel from './AgentReportPanel.svelte';
import type { AgentImportReport, AgentStageEvent } from './commands';
import { locale } from './i18n';

describe('AgentReportPanel public evidence boundary', () => {
  beforeEach(() => locale.set('en'));
  afterEach(() => {
    cleanup();
    locale.set('ckb');
  });

  it('never renders private path ancestry or persisted diagnostic text', () => {
    const privatePath = 'D:\\private\\Wareen\\meeting.wav';
    const privateTranscript = 'D:\\private\\Wareen\\meeting__provider.txt';
    const privateError = 'token=secret SELECT * FROM speech_segments';
    const report = {
      id: 'report-1',
      agentRunId: 'run-1',
      source: 'file',
      status: 'failed',
      summary: {
        totalSegments: 1,
        agenticReadiness: {
          status: 'blocked',
          ready: false,
          sourceReferenceModels: ['gemini-2.5-pro'],
          sourceReferenceModelCount: 1,
          availableHypothesisModels: ['omniasr-wsl-7b'],
          availableHypothesisModelCount: 1,
          requiredHypothesisModels: 2,
          checks: [
            {
              code: 'primary_asr',
              label: privatePath,
              status: 'blocked',
              detail: privateError,
            },
          ],
          checkCount: 1,
        },
        sourceReferences: [
          {
            audioFileLabel: privatePath,
            modelId: 'gemini-2.5-pro',
            audioContentHash: 'a'.repeat(64),
            audioSizeBytes: 123,
            transcriptFileLabel: privateTranscript,
            textChars: 42,
          },
        ],
        sourceReferenceCount: 1,
        sourceReferenceRequired: true,
        requiredSourceReferenceModels: ['gemini-2.5-pro'],
        requiredSourceReferenceModelCount: 1,
        sourceReferenceModels: ['gemini-2.5-pro'],
        sourceReferenceModelCount: 1,
        sourceReferenceCoverage: [
          {
            audioFileLabel: privatePath,
            requiredModels: ['gemini-2.5-pro'],
            requiredModelCount: 1,
            presentModels: [],
            presentModelCount: 0,
            missingModels: ['gemini-2.5-pro'],
            missingModelCount: 1,
            complete: false,
          },
        ],
        sourceReferenceCoverageCount: 1,
        longFileDossiers: [
          {
            audioFileLabel: privatePath,
            chunkCount: 1,
            totalDurationMs: 1000,
            sourceReferences: [],
            sourceReferenceCount: 0,
            sourceReferenceCoverage: {
              audioFileLabel: privatePath,
              requiredModels: [],
              requiredModelCount: 0,
              presentModels: [],
              presentModelCount: 0,
              missingModels: [],
              missingModelCount: 0,
              complete: false,
            },
            hypothesisModelCounts: {},
            hypothesisModelKindCount: 0,
            verdictCounts: {},
            verdictKindCount: 0,
            trainingReadySegments: 0,
            escalatedSegments: [],
            escalatedSegmentCount: 0,
            promotionStatus: 'blocked',
            promotionBlockerCodes: ['unknown'],
            promotionBlockerCount: 1,
            promotionBlockers: [privateError],
          },
        ],
        longFileDossierCount: 1,
        hypothesisModels: ['omniasr-wsl-7b'],
        hypothesisModelCount: 1,
        hypothesisModelCounts: { 'omniasr-wsl-7b': 1 },
        hypothesisModelKindCount: 1,
        verdictCounts: { unprocessed: 1 },
        verdictKindCount: 1,
        escalatedSegments: ['segment-1'],
        escalatedSegmentCount: 1,
        trainingGradeSummary: {
          totalSegments: 1,
          trainingReadySegments: 0,
          goldSegments: 0,
          silverSegments: 0,
          reviewSegments: 1,
          rejectedSegments: 0,
        },
        trainingGradeReasonCounts: { placeholder_transcript: 1 },
        trainingGradeReasonKindCount: 1,
        hypothesisCoverageBlockers: [],
        hypothesisCoverageBlockerCount: 0,
        orchestrationStages: [
          {
            stage: 'dataset_promotion',
            status: 'blocked',
            detailCode: 'blocked',
            summary: privateError,
            blockerCount: 1,
            blockers: [privatePath],
          },
        ],
        orchestrationStageCount: 1,
      },
      errorCode: 'IMPORT_REPORT_FAILED',
      error: privateError,
      createdAt: '2026-08-28T10:00:00Z',
    } as unknown as AgentImportReport;
    const stageEvents = [
      {
        id: 1,
        runId: 'run-1',
        source: 'file',
        stage: 'jury_adjudication',
        status: 'blocked',
        fileLabel: privatePath,
        detailCode: 'blocked',
        detail: privateError,
        current: 0,
        total: 1,
        createdAt: '2026-08-28T10:00:00Z',
      },
    ] as unknown as AgentStageEvent[];
    const { container } = render(AgentReportPanel, {
      report,
      stageEvents,
    });

    expect(container).toHaveTextContent('meeting.wav');
    expect(container).toHaveTextContent('This run stopped safely');
    expect(container.innerHTML).not.toMatch(
      /D:\\private|Wareen|token=secret|SELECT|speech_segments/,
    );
  });
});
