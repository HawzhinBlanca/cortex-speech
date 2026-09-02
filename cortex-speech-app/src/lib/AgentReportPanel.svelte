<script lang="ts">
  import type { AgentImportReport, AgentStageEvent } from './commands';
  import { t } from './i18n';
  import AgentReportDecisionEvidence from './AgentReportDecisionEvidence.svelte';
  import AgentReportEvidence from './AgentReportEvidence.svelte';
  import {
    agentCheckLabel,
    agentStageTone,
    agentStatusLabel,
    compactAgentReportModels,
    formatAgentReportDate,
    formatAgentReportPercent,
    publicAgentReportIdentifier,
    topAgentReportCounts,
  } from './agentReportPresentation';

  let {
    report,
    stageEvents = [],
  }: { report: AgentImportReport | null; stageEvents?: AgentStageEvent[] } = $props();

  const countFor = (key: string): number => report?.summary.verdictCounts[key] ?? 0;
  const fmtDate = (value: string): string => formatAgentReportDate(value, $t);
  const pct = (ready: number, total: number): string => formatAgentReportPercent(ready, total);
  const compactModels = (models: string[], total: number): string =>
    compactAgentReportModels(models, total, $t);
  const publicIdentifier = (value: string): string => publicAgentReportIdentifier(value);
  const topCounts = (
    counts: Record<string, number> | undefined,
    limit: number,
  ): Array<[string, number]> => topAgentReportCounts(counts, limit);

  const stageTone = (status: string): string => agentStageTone(status);
  const statusLabel = (status: string): string => agentStatusLabel(status, $t);
  const checkLabel = (id: string): string => agentCheckLabel(id, $t);
</script>

{#if report}
  <section class="card p-4 space-y-3" data-testid="agent-report-panel">
    <div class="flex items-start justify-between gap-3">
      <div class="min-w-0">
        <h2 class="text-sm font-semibold text-cortex-200 uppercase tracking-wider">
          {$t('agentReport.title')}
        </h2>
        <div class="mt-1 text-[10px] text-cortex-500 truncate" title={fmtDate(report.createdAt)}>
          {$t('agentReport.created')}: <bdi dir="ltr">{fmtDate(report.createdAt)}</bdi>
        </div>
      </div>
      <span
        class={`text-[10px] px-2 py-1 rounded border font-mono shrink-0 ${
          report.status === 'completed'
            ? 'bg-emerald-950/50 text-emerald-300 border-emerald-800/40'
            : 'bg-red-950/50 text-red-300 border-red-800/40'
        }`}
      >
        {statusLabel(report.status)}
      </span>
    </div>

    <div class="grid grid-cols-2 gap-2">
      <div class="bg-cortex-800/30 rounded-lg p-2">
        <div class="text-lg font-bold text-cortex-200">{report.summary.totalSegments}</div>
        <div class="text-[10px] text-cortex-400">{$t('agentReport.segments')}</div>
      </div>
      <div class="bg-cortex-800/30 rounded-lg p-2">
        <div class="text-lg font-bold text-emerald-300" data-testid="agent-report-training-ready">
          {report.summary.trainingGradeSummary.trainingReadySegments}
          <span class="text-[10px] text-cortex-500">
            / {pct(
              report.summary.trainingGradeSummary.trainingReadySegments,
              report.summary.totalSegments,
            )}
          </span>
        </div>
        <div class="text-[10px] text-cortex-400">{$t('agentReport.trainingReady')}</div>
      </div>
      <div class="bg-cortex-800/30 rounded-lg p-2">
        <div class="text-lg font-bold text-cyan-300" data-testid="agent-report-source-ref-count">
          {report.summary.sourceReferenceCount}
        </div>
        <div class="text-[10px] text-cortex-400">{$t('agentReport.sourceRefs')}</div>
      </div>
      <div class="bg-cortex-800/30 rounded-lg p-2">
        <div class="text-lg font-bold text-amber-300" data-testid="agent-report-escalated-count">
          {report.summary.escalatedSegmentCount}
        </div>
        <div class="text-[10px] text-cortex-400">{$t('agentReport.reviewQueue')}</div>
      </div>
    </div>

    <div class="space-y-1 text-[10px] text-cortex-400">
      <div class="flex justify-between gap-2">
        <span>{$t('agentReport.referenceModels')}</span>
        <span
          dir="ltr"
          class="text-cortex-200 text-end truncate"
          title={compactModels(
            report.summary.sourceReferenceModels,
            report.summary.sourceReferenceModelCount,
          )}
          data-testid="agent-report-source-reference-models"
        >
          {compactModels(
            report.summary.sourceReferenceModels,
            report.summary.sourceReferenceModelCount,
          )}
        </span>
      </div>
      <div class="flex justify-between gap-2">
        <span>{$t('agentReport.requiredReferenceModels')}</span>
        <span
          dir="ltr"
          class="text-cortex-200 text-end truncate"
          title={compactModels(
            report.summary.requiredSourceReferenceModels,
            report.summary.requiredSourceReferenceModelCount,
          )}
          data-testid="agent-report-required-reference-models"
        >
          {compactModels(
            report.summary.requiredSourceReferenceModels,
            report.summary.requiredSourceReferenceModelCount,
          )}
        </span>
      </div>
      <div class="flex justify-between gap-2">
        <span>{$t('agentReport.hypothesisModels')}</span>
        <span
          dir="ltr"
          class="text-cortex-200 text-end truncate"
          title={compactModels(
            report.summary.hypothesisModels,
            report.summary.hypothesisModelCount,
          )}
          data-testid="agent-report-hypothesis-models"
        >
          {compactModels(report.summary.hypothesisModels, report.summary.hypothesisModelCount)}
        </span>
      </div>
    </div>

    {#if report.summary.agenticReadiness}
      <div
        class="rounded bg-cortex-900/40 border border-cortex-800/40 p-2 space-y-2"
        data-testid="agent-report-agentic-readiness"
      >
        <div class="flex items-center justify-between gap-2">
          <div class="text-[10px] text-cortex-500 uppercase tracking-wider">
            {$t('agentReport.agenticReadiness')}
          </div>
          <span
            class={`font-mono border rounded px-1.5 py-0.5 text-[10px] shrink-0 ${stageTone(
              report.summary.agenticReadiness.status,
            )}`}
          >
            {statusLabel(report.summary.agenticReadiness.status)}
          </span>
        </div>
        <div class="space-y-1 text-[10px] text-cortex-400">
          <div class="flex justify-between gap-2">
            <span>{$t('agentReport.referenceModels')}</span>
            <span
              dir="ltr"
              class="text-cortex-200 text-end truncate"
              title={compactModels(
                report.summary.agenticReadiness.sourceReferenceModels,
                report.summary.agenticReadiness.sourceReferenceModelCount,
              )}
              data-testid="agent-report-ready-reference-models"
            >
              {compactModels(
                report.summary.agenticReadiness.sourceReferenceModels,
                report.summary.agenticReadiness.sourceReferenceModelCount,
              )}
            </span>
          </div>
          <div class="flex justify-between gap-2">
            <span>{$t('agentReport.readyHypothesisModels')}</span>
            <span
              dir="ltr"
              class="text-cortex-200 text-end truncate"
              title={compactModels(
                report.summary.agenticReadiness.availableHypothesisModels,
                report.summary.agenticReadiness.availableHypothesisModelCount,
              )}
              data-testid="agent-report-ready-hypothesis-models"
            >
              {compactModels(
                report.summary.agenticReadiness.availableHypothesisModels,
                report.summary.agenticReadiness.availableHypothesisModelCount,
              )}
            </span>
          </div>
          <div class="flex justify-between gap-2">
            <span>{$t('agentReport.requiredHypothesisCount')}</span>
            <span class="text-cortex-200 font-mono">
              {report.summary.agenticReadiness.availableHypothesisModelCount}/{report.summary
                .agenticReadiness.requiredHypothesisModels}
            </span>
          </div>
        </div>
        <div class="space-y-1">
          {#each report.summary.agenticReadiness.checks.slice(0, 4) as check}
            <div class="space-y-0.5 text-[10px]" title={statusLabel(check.status)}>
              <div class="grid grid-cols-[minmax(0,1fr)_auto] gap-2">
                <span class="text-cortex-300 truncate">{checkLabel(check.code)}</span>
                <span
                  class={`font-mono border rounded px-1 py-0.5 shrink-0 ${stageTone(check.status)}`}
                >
                  {statusLabel(check.status)}
                </span>
              </div>
            </div>
          {/each}
        </div>
      </div>
    {/if}

    {#if topCounts(report.summary.hypothesisModelCounts, 4).length}
      <div
        class="rounded bg-cortex-900/40 border border-cortex-800/40 p-2 space-y-1"
        data-testid="agent-report-model-coverage"
      >
        <div class="text-[10px] text-cortex-500 uppercase tracking-wider">
          {$t('agentReport.modelCoverage')}
        </div>
        {#each topCounts(report.summary.hypothesisModelCounts, 4) as [model, count]}
          <div class="flex justify-between gap-2 text-[10px]">
            <span class="text-cortex-300 font-mono truncate">{publicIdentifier(model)}</span>
            <span class="text-cortex-200 shrink-0">
              {count}/{report.summary.totalSegments}
            </span>
          </div>
        {/each}
      </div>
    {/if}

    <AgentReportEvidence {report} {stageEvents} />

    {#if report.summary.escalatedSegmentCount}
      <div
        class="rounded bg-amber-950/20 border border-amber-900/30 p-2 space-y-1"
        data-testid="agent-report-escalated-ids"
      >
        <div class="text-[10px] text-amber-300 uppercase tracking-wider">
          {$t('agentReport.escalatedIds')}
        </div>
        <div class="text-[10px] text-cortex-300 font-mono break-all">
          {report.summary.escalatedSegments
            .slice(0, 6)
            .map(publicIdentifier)
            .filter(Boolean)
            .join(', ')}
          {#if report.summary.escalatedSegmentCount > 6}
            {$t('agentReport.more', { count: String(report.summary.escalatedSegmentCount - 6) })}
          {/if}
        </div>
      </div>
    {/if}

    <AgentReportDecisionEvidence {report} />

    <div class="grid grid-cols-4 gap-1 text-center">
      <div class="rounded bg-cortex-900/50 border border-cortex-800/30 px-1.5 py-2">
        <div class="text-xs font-semibold text-emerald-300">{countFor('jury_accept')}</div>
        <div class="text-[9px] text-cortex-500">{$t('agentReport.jury')}</div>
      </div>
      <div class="rounded bg-cortex-900/50 border border-cortex-800/30 px-1.5 py-2">
        <div class="text-xs font-semibold text-cyan-300">{countFor('auto_accept')}</div>
        <div class="text-[9px] text-cortex-500">{$t('agentReport.automatic')}</div>
      </div>
      <div class="rounded bg-cortex-900/50 border border-cortex-800/30 px-1.5 py-2">
        <div class="text-xs font-semibold text-amber-300">{countFor('escalated')}</div>
        <div class="text-[9px] text-cortex-500">{$t('agentReport.review')}</div>
      </div>
      <div class="rounded bg-cortex-900/50 border border-cortex-800/30 px-1.5 py-2">
        <div class="text-xs font-semibold text-cortex-300">{countFor('unprocessed')}</div>
        <div class="text-[9px] text-cortex-500">{$t('agentReport.open')}</div>
      </div>
    </div>

    {#if report.errorCode}
      <div class="text-[10px] text-red-300 bg-red-950/30 border border-red-900/40 rounded p-2">
        <span dir="auto">{$t('agentReport.runFailedDetail')}</span>
      </div>
    {/if}
  </section>
{/if}
