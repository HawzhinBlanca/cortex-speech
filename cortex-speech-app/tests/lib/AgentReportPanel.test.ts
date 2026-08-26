import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import AgentReportPanel from '../../src/lib/AgentReportPanel.svelte';
import { locale } from '../../src/lib/i18n';
import { en } from '../../src/lib/i18n/en';
import type { AgentImportReport, AgentStageEvent } from '../../src/lib/commands';

function makeReport(overrides: Partial<AgentImportReport> = {}): AgentImportReport {
  return {
    id: 'report-1',
    agentRunId: 'run-1',
    source: 'file',
    status: 'completed',
    audioPaths: ['C:\\audio\\long.wav'],
    segmentIds: ['seg-1', 'seg-2'],
    summary: {
      totalSegments: 2,
      agenticReadiness: {
        status: 'blocked',
        ready: false,
        sourceReferenceModels: ['gemini-2.5-pro'],
        availableHypothesisModels: ['omniasr-wsl-7b'],
        requiredHypothesisModels: 2,
        checks: [
          {
            id: 'source_reference',
            label: 'Whole-file source references',
            status: 'ready',
            detail: 'Configured source-reference model: gemini-2.5-pro',
          },
          {
            id: 'hypothesis_coverage',
            label: 'Multi-model hypothesis coverage',
            status: 'blocked',
            detail: 'Only one usable hypothesis model is ready.',
          },
        ],
      },
      sourceReferences: [
        {
          audioPath: 'C:\\audio\\long.wav',
          modelId: 'gemini-2.5-pro',
          transcriptPath: 'source_transcripts\\long__gemini.txt',
          textChars: 1200,
        },
      ],
      sourceReferenceRequired: true,
      requiredSourceReferenceModels: ['gemini-2.5-pro'],
      sourceReferenceModels: ['gemini-2.5-pro'],
      sourceReferenceCoverage: [
        {
          audioPath: 'C:\\audio\\long.wav',
          requiredModels: ['gemini-2.5-pro'],
          presentModels: ['gemini-2.5-pro'],
          missingModels: [],
          complete: true,
        },
      ],
      longFileDossiers: [
        {
          audioPath: 'C:\\audio\\long.wav',
          chunkCount: 2,
          totalDurationMs: 32000,
          sourceReferences: [
            {
              audioPath: 'C:\\audio\\long.wav',
              modelId: 'gemini-2.5-pro',
              transcriptPath: 'source_transcripts\\long__gemini.txt',
              textChars: 1200,
            },
          ],
          sourceReferenceCoverage: {
            audioPath: 'C:\\audio\\long.wav',
            requiredModels: ['gemini-2.5-pro'],
            presentModels: ['gemini-2.5-pro'],
            missingModels: [],
            complete: true,
          },
          hypothesisModelCounts: {
            'omniasr-ctc-300m': 2,
            'omniasr-wsl-7b': 1,
          },
          verdictCounts: {
            jury_accept: 1,
            escalated: 1,
          },
          trainingReadySegments: 1,
          escalatedSegments: ['seg-2'],
          promotionStatus: 'needs_review',
          promotionBlockers: ['seg-2'],
        },
      ],
      hypothesisModels: ['omniasr-ctc-300m', 'omniasr-wsl-7b'],
      hypothesisModelCounts: {
        'omniasr-ctc-300m': 2,
        'omniasr-wsl-7b': 1,
      },
      verdictCounts: {
        jury_accept: 1,
        escalated: 1,
      },
      escalatedSegments: ['seg-2'],
      hypothesisCoverageBlockers: [
        {
          segmentId: 'seg-2',
          grade: 'review',
          trainingReady: false,
          coverage: {
            minimumNonEmptyModelCount: 2,
            nonEmptyModelCount: 1,
            passesMinimum: false,
            nonEmptyModels: ['omniasr-wsl-7b'],
            ignoredModels: ['asr'],
          },
        },
      ],
      orchestrationStages: [
        {
          stage: 'source_reference',
          status: 'ready',
          summary: '1 whole-file source reference transcript recorded.',
          blockerCount: 0,
          blockers: [],
        },
        {
          stage: 'multi_model_hypotheses',
          status: 'blocked',
          summary: '1 segment lacks enough model hypotheses.',
          blockerCount: 1,
          blockers: ['seg-2'],
        },
        {
          stage: 'dataset_promotion',
          status: 'needs_review',
          summary: '1 segment is training-ready, but review queue items remain.',
          blockerCount: 1,
          blockers: ['seg-2'],
        },
      ],
      trainingGradeSummary: {
        totalSegments: 2,
        trainingReadySegments: 1,
        goldSegments: 0,
        silverSegments: 1,
        reviewSegments: 1,
        rejectedSegments: 0,
      },
      trainingGradeReasonCounts: {
        high_confidence_jury_accept: 1,
        jury_accept_needs_review: 1,
      },
    },
    juryReport: { referenceCommitted: 1, humanInbox: 1 },
    error: null,
    createdAt: '2026-06-16T12:00:00Z',
    ...overrides,
  };
}

function makeStageEvents(): AgentStageEvent[] {
  return [
    {
      id: 1,
      runId: 'run-1',
      source: 'file',
      stage: 'source_reference',
      status: 'completed',
      file: 'long.wav',
      detail: '2 whole-file source reference transcripts recorded',
      current: 1,
      total: 1,
      createdAt: '2026-06-16T12:00:01Z',
    },
    {
      id: 2,
      runId: 'run-1',
      source: 'file',
      stage: 'jury_adjudication',
      status: 'blocked',
      file: 'long.wav',
      detail: '1 segment lacks enough model hypotheses',
      current: 0,
      total: 2,
      createdAt: '2026-06-16T12:00:02Z',
    },
  ];
}

describe('AgentReportPanel', () => {
  beforeEach(() => {
    locale.set('en');
  });

  afterEach(() => {
    cleanup();
  });

  it('renders the latest multi-agent import summary', () => {
    render(AgentReportPanel, { props: { report: makeReport(), stageEvents: makeStageEvents() } });

    expect(screen.getByTestId('agent-report-panel')).toBeInTheDocument();
    expect(screen.getByText('Latest Agent Run')).toBeInTheDocument();
    expect(screen.getByText(en['agentReport.status.completed'])).toBeInTheDocument();
    expect(screen.getByText('Training-ready')).toBeInTheDocument();
    expect(screen.getByTestId('agent-report-training-ready')).toHaveTextContent('1 / 50%');
    expect(screen.getAllByText('gemini-2.5-pro')).toHaveLength(4);
    expect(screen.getByText('omniasr-ctc-300m, omniasr-wsl-7b')).toBeInTheDocument();
    expect(screen.getByTestId('agent-report-agentic-readiness')).toHaveTextContent(
      'Agentic readiness',
    );
    expect(screen.getByTestId('agent-report-agentic-readiness')).toHaveTextContent(
      en['agentReport.status.blocked'],
    );
    expect(screen.getByTestId('agent-report-agentic-readiness')).toHaveTextContent(
      'Ready hypothesis models',
    );
    expect(screen.getByTestId('agent-report-agentic-readiness')).toHaveTextContent(
      'omniasr-wsl-7b',
    );
    expect(screen.getByTestId('agent-report-agentic-readiness')).toHaveTextContent('1/2');
    expect(screen.getByTestId('agent-report-agentic-readiness')).toHaveTextContent(
      'Multi-model hypothesis coverage',
    );
    expect(screen.getByTestId('agent-report-agentic-readiness')).toHaveTextContent(
      'Only one usable hypothesis model is ready.',
    );
    expect(screen.getByTestId('agent-report-model-coverage')).toHaveTextContent('omniasr-ctc-300m');
    expect(screen.getByTestId('agent-report-model-coverage')).toHaveTextContent('2/2');
    expect(screen.getByTestId('agent-report-source-reference-coverage')).toHaveTextContent(
      'long.wav',
    );
    expect(screen.getByTestId('agent-report-source-reference-coverage')).toHaveTextContent('1/1');
    expect(screen.getByTestId('agent-report-long-file-dossiers')).toHaveTextContent('long.wav');
    expect(screen.getByTestId('agent-report-long-file-dossiers')).toHaveTextContent(
      en['agentReport.status.needsReview'],
    );
    expect(screen.getByTestId('agent-report-long-file-dossiers')).toHaveTextContent('2 chunks');
    expect(screen.getByTestId('agent-report-long-file-dossiers')).toHaveTextContent('1 ready');
    expect(screen.getByTestId('agent-report-persisted-stage-events')).toHaveTextContent(
      'Persisted stage log',
    );
    expect(screen.getByTestId('agent-report-persisted-stage-events')).toHaveTextContent(
      'source_reference',
    );
    expect(screen.getByTestId('agent-report-persisted-stage-events')).toHaveTextContent(
      'jury_adjudication',
    );
    expect(screen.getByTestId('agent-report-persisted-stage-events')).toHaveTextContent(
      `${en['agentReport.status.blocked']} 0/2`,
    );
    expect(screen.getByTestId('agent-report-source-files')).toHaveTextContent('long__gemini.txt');
    expect(screen.getByTestId('agent-report-source-files')).toHaveTextContent('1200 chars');
    expect(screen.getByTestId('agent-report-orchestration-stages')).toHaveTextContent(
      'source_reference',
    );
    expect(screen.getByTestId('agent-report-orchestration-stages')).toHaveTextContent(
      en['agentReport.status.blocked'],
    );
    expect(screen.getByTestId('agent-report-orchestration-stages')).toHaveTextContent(
      en['agentReport.status.needsReview'],
    );
    expect(screen.getByTestId('agent-report-coverage-blockers')).toHaveTextContent('seg-2');
    expect(screen.getByTestId('agent-report-coverage-blockers')).toHaveTextContent('1/2');
    expect(screen.getByTestId('agent-report-grade-reasons')).toHaveTextContent(
      'jury_accept_needs_review',
    );
    expect(screen.getByTestId('agent-report-escalated-ids')).toHaveTextContent('seg-2');
  });

  it('shows failed report errors', () => {
    render(AgentReportPanel, {
      props: {
        report: makeReport({
          status: 'failed',
          error: 'Post-import jury adjudication failed after single-file import',
        }),
      },
    });

    expect(screen.getByText(en['agentReport.status.failed'])).toBeInTheDocument();
    expect(
      screen.getByText('Post-import jury adjudication failed after single-file import'),
    ).toBeInTheDocument();
  });
});
