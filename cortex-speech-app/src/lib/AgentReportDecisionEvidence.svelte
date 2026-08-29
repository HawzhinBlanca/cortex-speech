<script lang="ts">
  import type { AgentImportReport } from './commands';
  import { t } from './i18n';
  import {
    agentStageLabel,
    agentStageTone,
    agentStatusLabel,
    publicAgentReportIdentifier,
    topAgentReportCounts,
  } from './agentReportPresentation';

  let { report }: { report: AgentImportReport } = $props();
  const stageTone = (status: string): string => agentStageTone(status);
  const statusLabel = (status: string): string => agentStatusLabel(status, $t);
  const stageLabel = (stage: string): string => agentStageLabel(stage, $t);
  const publicIdentifier = (value: string): string => publicAgentReportIdentifier(value);
</script>

{#if report.summary.orchestrationStageCount}
  <div
    class="rounded bg-cortex-900/40 border border-cortex-800/40 p-2 space-y-1"
    data-testid="agent-report-orchestration-stages"
  >
    <div class="text-[10px] text-cortex-500 uppercase tracking-wider">
      {$t('agentReport.orchestrationStages')}
    </div>
    {#each report.summary.orchestrationStages.slice(0, 5) as stage}
      <div
        class="grid grid-cols-[minmax(0,1fr)_auto] gap-2 text-[10px]"
        title={statusLabel(stage.status)}
      >
        <bdi class="text-cortex-300 truncate" dir="auto">{stageLabel(stage.stage)}</bdi>
        <span class={`font-mono border rounded px-1 py-0.5 shrink-0 ${stageTone(stage.status)}`}>
          {statusLabel(stage.status)}
          {#if stage.blockerCount}- {stage.blockerCount}{/if}
        </span>
      </div>
    {/each}
  </div>
{/if}

{#if report.summary.hypothesisCoverageBlockerCount}
  <div
    class="rounded bg-amber-950/20 border border-amber-900/30 p-2 space-y-1"
    data-testid="agent-report-coverage-blockers"
  >
    <div class="flex justify-between gap-2 text-[10px] text-amber-300 uppercase tracking-wider">
      <span>{$t('agentReport.coverageBlockers')}</span>
      <span class="font-mono">{report.summary.hypothesisCoverageBlockerCount}</span>
    </div>
    {#each report.summary.hypothesisCoverageBlockers.slice(0, 4) as blocker}
      <div class="flex justify-between gap-2 text-[10px]">
        <span class="text-cortex-300 font-mono truncate" title={publicIdentifier(blocker.segmentId)}
          >{publicIdentifier(blocker.segmentId)}</span
        >
        <span class="text-cortex-200 shrink-0">
          {blocker.coverage.nonEmptyModelCount}/{blocker.coverage.minimumNonEmptyModelCount}
        </span>
      </div>
    {/each}
    {#if report.summary.hypothesisCoverageBlockerCount > 4}
      <div class="text-[10px] text-cortex-500 text-end">
        {$t('agentReport.more', {
          count: String(report.summary.hypothesisCoverageBlockerCount - 4),
        })}
      </div>
    {/if}
  </div>
{/if}

{#if topAgentReportCounts(report.summary.trainingGradeReasonCounts, 4).length}
  <div
    class="rounded bg-cortex-900/40 border border-cortex-800/40 p-2 space-y-1"
    data-testid="agent-report-grade-reasons"
  >
    <div class="text-[10px] text-cortex-500 uppercase tracking-wider">
      {$t('agentReport.gradeReasons')}
    </div>
    {#each topAgentReportCounts(report.summary.trainingGradeReasonCounts, 4) as [reason, count]}
      <div class="flex justify-between gap-2 text-[10px]">
        <bdi class="text-cortex-300 truncate" dir="auto" title={publicIdentifier(reason)}
          >{publicIdentifier(reason)}</bdi
        >
        <span class="text-cortex-200 font-mono shrink-0">{count}</span>
      </div>
    {/each}
  </div>
{/if}
