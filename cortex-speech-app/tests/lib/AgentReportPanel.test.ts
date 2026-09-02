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
    summary: {
      totalSegments: 2,
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
            code: 'source_reference',
            status: 'ready',
          },
          {
            code: 'hypothesis_coverage',
            status: 'blocked',
          },
        ],
        checkCount: 2,
      },
      sourceReferences: [
        {
          audioFileLabel: 'long.wav',
          modelId: 'gemini-2.5-pro',
          audioContentHash: null,
          audioSizeBytes: null,
          transcriptFileLabel: 'long__gemini.txt',
          textChars: 1200,
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
          audioFileLabel: 'long.wav',
          requiredModels: ['gemini-2.5-pro'],
          requiredModelCount: 1,
          presentModels: ['gemini-2.5-pro'],
          presentModelCount: 1,
          missingModels: [],
          missingModelCount: 0,
          complete: true,
        },
      ],
      sourceReferenceCoverageCount: 1,
      longFileDossiers: [
        {
          audioFileLabel: 'long.wav',
          chunkCount: 2,
          totalDurationMs: 32000,
          sourceReferences: [
            {
              audioFileLabel: 'long.wav',
              modelId: 'gemini-2.5-pro',
              audioContentHash: null,
              audioSizeBytes: null,
              transcriptFileLabel: 'long__gemini.txt',
              textChars: 1200,
            },
          ],
          sourceReferenceCount: 1,
          sourceReferenceCoverage: {
            audioFileLabel: 'long.wav',
            requiredModels: ['gemini-2.5-pro'],
            requiredModelCount: 1,
            presentModels: ['gemini-2.5-pro'],
            presentModelCount: 1,
            missingModels: [],
            missingModelCount: 0,
            complete: true,
          },
          hypothesisModelCounts: {
            'omniasr-ctc-300m': 2,
            'omniasr-wsl-7b': 1,
          },
          hypothesisModelKindCount: 2,
          verdictCounts: {
            jury_accept: 1,
            escalated: 1,
          },
          verdictKindCount: 2,
          trainingReadySegments: 1,
          escalatedSegments: ['seg-2'],
          escalatedSegmentCount: 1,
          promotionStatus: 'needs_review',
          promotionBlockerCodes: ['missing_hypothesis_coverage'],
          promotionBlockerCount: 1,
        },
      ],
      longFileDossierCount: 1,
      hypothesisModels: ['omniasr-ctc-300m', 'omniasr-wsl-7b'],
      hypothesisModelCount: 2,
      hypothesisModelCounts: {
        'omniasr-ctc-300m': 2,
        'omniasr-wsl-7b': 1,
      },
      hypothesisModelKindCount: 2,
      verdictCounts: {
        jury_accept: 1,
        escalated: 1,
      },
      verdictKindCount: 2,
      escalatedSegments: ['seg-2'],
      escalatedSegmentCount: 1,
      hypothesisCoverageBlockers: [
        {
          segmentId: 'seg-2',
          grade: 'review',
          trainingReady: false,
          coverage: {
            minimumNonEmptyModelCount: 2,
            nonEmptyModelCount: 1,
            passesMinimum: false,
          },
        },
      ],
      hypothesisCoverageBlockerCount: 1,
      orchestrationStages: [
        {
          stage: 'source_reference',
          status: 'ready',
          detailCode: 'ready',
          blockerCount: 0,
        },
        {
          stage: 'multi_model_hypotheses',
          status: 'blocked',
          detailCode: 'blocked',
          blockerCount: 1,
        },
        {
          stage: 'dataset_promotion',
          status: 'needs_review',
          detailCode: 'needs_review',
          blockerCount: 1,
        },
      ],
      orchestrationStageCount: 3,
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
      trainingGradeReasonKindCount: 2,
    },
    errorCode: null,
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
      fileLabel: 'long.wav',
      detailCode: 'completed',
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
      fileLabel: 'long.wav',
      detailCode: 'blocked',
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
    expect(screen.getAllByText(en['agentReport.status.completed']).length).toBeGreaterThan(0);
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
      en['agentReport.status.blocked'],
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
      'Source reference',
    );
    expect(screen.getByTestId('agent-report-persisted-stage-events')).toHaveTextContent(
      'Jury adjudication',
    );
    expect(screen.getByTestId('agent-report-persisted-stage-events')).toHaveTextContent(
      `${en['agentReport.status.blocked']} 0/2`,
    );
    expect(screen.getByTestId('agent-report-source-files')).toHaveTextContent('long__gemini.txt');
    expect(screen.getByTestId('agent-report-source-files')).toHaveTextContent('1200 chars');
    expect(screen.getByTestId('agent-report-orchestration-stages')).toHaveTextContent(
      'Source reference',
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
          errorCode: 'IMPORT_REPORT_FAILED',
        }),
      },
    });

    expect(screen.getByText(en['agentReport.status.failed'])).toBeInTheDocument();
    expect(screen.getByText(en['agentReport.runFailedDetail'])).toBeInTheDocument();
  });

  it('renders authoritative totals instead of bounded preview lengths', () => {
    const report = makeReport();
    report.summary.sourceReferenceCount = 10_000;
    report.summary.sourceReferenceModelCount = 10_000;
    report.summary.requiredSourceReferenceModelCount = 9_998;
    report.summary.hypothesisModelCount = 9_999;
    report.summary.agenticReadiness!.sourceReferenceModelCount = 9_997;
    report.summary.agenticReadiness!.availableHypothesisModelCount = 9_996;
    report.summary.escalatedSegmentCount = 9;
    report.summary.sourceReferenceCoverageCount = 11;
    report.summary.sourceReferenceCoverage[0].complete = false;
    report.summary.sourceReferenceCoverage[0].missingModels = ['model-missing'];
    report.summary.sourceReferenceCoverage[0].missingModelCount = 9_995;
    report.summary.longFileDossierCount = 12;
    report.summary.hypothesisCoverageBlockerCount = 13;

    render(AgentReportPanel, { props: { report } });

    expect(screen.getByTestId('agent-report-source-ref-count')).toHaveTextContent('10000');
    expect(screen.getByTestId('agent-report-source-reference-models')).toHaveTextContent(
      'gemini-2.5-pro +9999',
    );
    expect(screen.getByTestId('agent-report-required-reference-models')).toHaveTextContent(
      'gemini-2.5-pro +9997',
    );
    expect(screen.getByTestId('agent-report-hypothesis-models')).toHaveTextContent('+9997');
    expect(screen.getByTestId('agent-report-ready-reference-models')).toHaveTextContent('+9996');
    expect(screen.getByTestId('agent-report-ready-hypothesis-models')).toHaveTextContent('+9995');
    expect(screen.getByTestId('agent-report-missing-models')).toHaveAttribute(
      'title',
      'model-missing +9994',
    );
    expect(screen.getByTestId('agent-report-escalated-count')).toHaveTextContent('9');
    expect(screen.getByTestId('agent-report-escalated-ids')).toHaveTextContent('+3 more');
    expect(screen.getByTestId('agent-report-source-reference-coverage')).toHaveTextContent(
      '+7 more',
    );
    expect(screen.getByTestId('agent-report-long-file-dossiers')).toHaveTextContent('+9 more');
    expect(screen.getByTestId('agent-report-coverage-blockers')).toHaveTextContent('+9 more');
  });
});
