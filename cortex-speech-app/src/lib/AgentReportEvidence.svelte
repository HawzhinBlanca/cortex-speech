<script lang="ts">
  import type { AgentImportReport, AgentStageEvent } from './commands';
  import { t } from './i18n';
  import {
    agentStageLabel,
    agentStageTone,
    agentStatusLabel,
    compactAgentReportModels,
    formatSourceReferenceIdentity,
    publicAgentReportIdentifier,
    publicAgentReportFileLabel,
  } from './agentReportPresentation';

  let { report, stageEvents = [] }: { report: AgentImportReport; stageEvents?: AgentStageEvent[] } =
    $props();

  const compactModels = (models: string[], total: number): string =>
    compactAgentReportModels(models, total, $t);
  const shortPath = (value: string): string => publicAgentReportFileLabel(value, $t);
  const sourceReferenceIdentity = (
    reference: AgentImportReport['summary']['sourceReferences'][number],
  ): string => formatSourceReferenceIdentity(reference, $t);
  const modelIdentifier = (value: string): string =>
    publicAgentReportIdentifier(value) || $t('agentReport.unknown');
  const stageTone = (status: string): string => agentStageTone(status);
  const statusLabel = (status: string): string => agentStatusLabel(status, $t);
  const stageLabel = (stage: string): string => agentStageLabel(stage, $t);
</script>

{#if report.summary.sourceReferenceCoverageCount}
  <div
    class="rounded bg-cortex-900/40 border border-cortex-800/40 p-2 space-y-1"
    data-testid="agent-report-source-reference-coverage"
  >
    <div class="text-[10px] text-cortex-500 uppercase tracking-wider">
      {$t('agentReport.referenceCoverage')}
    </div>
    {#each report.summary.sourceReferenceCoverage.slice(0, 4) as coverage}
      <div
        class="grid grid-cols-[minmax(0,1fr)_auto] gap-2 text-[10px]"
        title={shortPath(coverage.audioFileLabel)}
      >
        <bdi class="text-cortex-300 truncate" dir="auto">{shortPath(coverage.audioFileLabel)}</bdi>
        <span
          class={`font-mono border rounded px-1 py-0.5 shrink-0 ${
            coverage.complete
              ? 'text-emerald-300 bg-emerald-950/30 border-emerald-800/40'
              : 'text-red-300 bg-red-950/30 border-red-800/40'
          }`}
          title={compactModels(coverage.missingModels, coverage.missingModelCount)}
          data-testid="agent-report-missing-models"
        >
          {coverage.presentModelCount}/{coverage.requiredModelCount || coverage.presentModelCount}
          {#if !coverage.complete}
            {$t('agentReport.missing')}
          {/if}
        </span>
      </div>
    {/each}
    {#if report.summary.sourceReferenceCoverageCount > 4}
      <div class="text-[10px] text-cortex-500 text-end">
        {$t('agentReport.more', {
          count: String(report.summary.sourceReferenceCoverageCount - 4),
        })}
      </div>
    {/if}
  </div>
{/if}

{#if report.summary.longFileDossierCount}
  <div
    class="rounded bg-cortex-900/40 border border-cortex-800/40 p-2 space-y-1"
    data-testid="agent-report-long-file-dossiers"
  >
    <div class="text-[10px] text-cortex-500 uppercase tracking-wider">
      {$t('agentReport.longFileDossiers')}
    </div>
    {#each report.summary.longFileDossiers.slice(0, 3) as dossier}
      <div class="space-y-1 text-[10px]" title={shortPath(dossier.audioFileLabel)}>
        <div class="grid grid-cols-[minmax(0,1fr)_auto] gap-2">
          <bdi class="text-cortex-300 truncate" dir="auto">{shortPath(dossier.audioFileLabel)}</bdi>
          <span
            class={`font-mono border rounded px-1 py-0.5 shrink-0 ${stageTone(dossier.promotionStatus)}`}
          >
            {statusLabel(dossier.promotionStatus)}
          </span>
        </div>
        <div class="flex justify-between gap-2 text-cortex-500">
          <span>
            {dossier.chunkCount}
            {$t('agentReport.chunks')} - {dossier.trainingReadySegments}
            {$t('agentReport.readyShort')}
          </span>
          <bdi
            class="truncate text-end"
            dir="auto"
            title={$t('agentReport.blockerCount', {
              count: String(dossier.promotionBlockerCount),
            })}
          >
            {#if dossier.promotionBlockerCount}
              {$t('agentReport.blockerCount', {
                count: String(dossier.promotionBlockerCount),
              })}
            {:else}
              {$t('agentReport.noBlockers')}
            {/if}
          </bdi>
        </div>
      </div>
    {/each}
    {#if report.summary.longFileDossierCount > 3}
      <div class="text-[10px] text-cortex-500 text-end">
        {$t('agentReport.more', { count: String(report.summary.longFileDossierCount - 3) })}
      </div>
    {/if}
  </div>
{/if}

{#if stageEvents.length}
  <div
    class="rounded bg-cortex-900/40 border border-cortex-800/40 p-2 space-y-1"
    data-testid="agent-report-persisted-stage-events"
  >
    <div class="text-[10px] text-cortex-500 uppercase tracking-wider">
      {$t('agentReport.persistedStages')}
    </div>
    {#each stageEvents.slice(-6) as event}
      <div
        class="space-y-0.5 text-[10px]"
        title={`${shortPath(event.fileLabel)}: ${statusLabel(event.status)}`}
      >
        <div class="grid grid-cols-[minmax(0,1fr)_auto] gap-2">
          <bdi class="text-cortex-300 truncate" dir="auto">{stageLabel(event.stage)}</bdi>
          <span class={`font-mono border rounded px-1 py-0.5 shrink-0 ${stageTone(event.status)}`}>
            {statusLabel(event.status)}
            {#if event.total}
              {event.current}/{event.total}
            {/if}
          </span>
        </div>
        <div class="text-cortex-500 truncate">{statusLabel(event.status)}</div>
      </div>
    {/each}
  </div>
{/if}

{#if report.summary.sourceReferenceCount}
  <div
    class="rounded bg-cortex-900/40 border border-cortex-800/40 p-2 space-y-1"
    data-testid="agent-report-source-files"
  >
    <div class="text-[10px] text-cortex-500 uppercase tracking-wider">
      {$t('agentReport.sourceFiles')}
    </div>
    {#each report.summary.sourceReferences.slice(0, 3) as reference}
      <div
        class="grid min-w-0 grid-cols-[minmax(0,0.45fr)_minmax(0,1fr)] gap-x-2 gap-y-0.5 text-[10px]"
        title={`${shortPath(reference.audioFileLabel)} | ${shortPath(reference.transcriptFileLabel)} | ${sourceReferenceIdentity(reference)}`}
      >
        <bdi
          class="row-span-2 min-w-0 truncate font-mono text-cyan-300"
          dir="ltr"
          title={modelIdentifier(reference.modelId)}>{modelIdentifier(reference.modelId)}</bdi
        >
        <bdi class="text-cortex-300 text-end truncate min-w-0" dir="auto">
          {shortPath(reference.transcriptFileLabel)} - {reference.textChars}
          {$t('agentReport.chars')}
        </bdi>
        {#if sourceReferenceIdentity(reference)}
          <bdi class="text-cortex-500 text-end truncate min-w-0" dir="ltr">
            {sourceReferenceIdentity(reference)}
          </bdi>
        {/if}
      </div>
    {/each}
    {#if report.summary.sourceReferenceCount > 3}
      <div class="text-[10px] text-cortex-500 text-end">
        {$t('agentReport.more', { count: String(report.summary.sourceReferenceCount - 3) })}
      </div>
    {/if}
  </div>
{/if}
