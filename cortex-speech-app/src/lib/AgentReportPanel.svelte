<script lang="ts">
  import type { AgentImportReport, AgentStageEvent } from './commands';
  import { t } from './i18n';

  let {
    report,
    stageEvents = [],
  }: { report: AgentImportReport | null; stageEvents?: AgentStageEvent[] } = $props();

  function countFor(key: string): number {
    return report?.summary.verdictCounts[key] ?? 0;
  }

  function fmtDate(value: string): string {
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return value;
    return date.toLocaleString();
  }

  function pct(ready: number, total: number): string {
    if (total <= 0) return '0%';
    return `${Math.round((ready / total) * 100)}%`;
  }

  function compactModels(models: string[]): string {
    if (!models.length) return $t('agentReport.none');
    return models.slice(0, 3).join(', ') + (models.length > 3 ? ` +${models.length - 3}` : '');
  }

  function shortPath(value: string): string {
    const parts = value.split(/[\\/]/).filter(Boolean);
    return parts.length ? parts[parts.length - 1] : value;
  }

  function sourceReferenceIdentity(
    reference: NonNullable<AgentImportReport>['summary']['sourceReferences'][number],
  ): string {
    const hash = reference.audioContentHash ? reference.audioContentHash.slice(0, 12) : '';
    const size =
      typeof reference.audioSizeBytes === 'number' && Number.isFinite(reference.audioSizeBytes)
        ? `${reference.audioSizeBytes} bytes`
        : '';
    return [hash ? `hash ${hash}` : '', size].filter(Boolean).join(' | ');
  }

  function topCounts(
    counts: Record<string, number> | undefined,
    limit: number,
  ): Array<[string, number]> {
    return Object.entries(counts ?? {})
      .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
      .slice(0, limit);
  }

  function coverageBlockers(): NonNullable<AgentImportReport>['summary']['hypothesisCoverageBlockers'] {
    return report?.summary.hypothesisCoverageBlockers ?? [];
  }

  function sourceReferenceCoverage(): NonNullable<AgentImportReport>['summary']['sourceReferenceCoverage'] {
    return report?.summary.sourceReferenceCoverage ?? [];
  }

  function longFileDossiers(): NonNullable<AgentImportReport>['summary']['longFileDossiers'] {
    return report?.summary.longFileDossiers ?? [];
  }

  function orchestrationStages(): NonNullable<AgentImportReport>['summary']['orchestrationStages'] {
    return report?.summary.orchestrationStages ?? [];
  }

  function recentStageEvents(): AgentStageEvent[] {
    return stageEvents ?? [];
  }

  function stageTone(status: string): string {
    // `not_required` is NEUTRAL, never emerald. An optional dependency the owner switched off is a
    // valid configuration, but it is not proven coverage, and painting both the same green meant a
    // reviewer could not tell them apart at a glance (deep audit 2026-08-05). Not amber either — this
    // is not a degradation to fix, so it must not read as a warning.
    if (status === 'not_required') return 'text-cortex-300 bg-cortex-800/40 border-cortex-700/40';
    if (status === 'ready') return 'text-emerald-300 bg-emerald-950/30 border-emerald-800/40';
    if (status === 'completed') return 'text-emerald-300 bg-emerald-950/30 border-emerald-800/40';
    if (status === 'running') return 'text-amber-300 bg-amber-950/30 border-amber-800/40';
    if (status === 'degraded') return 'text-amber-300 bg-amber-950/30 border-amber-800/40';
    if (status === 'needs_review') return 'text-amber-300 bg-amber-950/30 border-amber-800/40';
    if (status === 'blocked') return 'text-red-300 bg-red-950/30 border-red-800/40';
    return 'text-cortex-300 bg-cortex-900/50 border-cortex-800/40';
  }
</script>

{#if report}
  <section class="card p-4 space-y-3" data-testid="agent-report-panel">
    <div class="flex items-start justify-between gap-3">
      <div class="min-w-0">
        <h2 class="text-sm font-semibold text-cortex-200 uppercase tracking-wider">
          {$t('agentReport.title')}
        </h2>
        <div class="mt-1 text-[10px] text-cortex-500 truncate" title={fmtDate(report.createdAt)}>
          {$t('agentReport.created')}: {fmtDate(report.createdAt)}
        </div>
      </div>
      <span
        class={`text-[10px] px-2 py-1 rounded border font-mono shrink-0 ${
          report.status === 'completed'
            ? 'bg-emerald-950/50 text-emerald-300 border-emerald-800/40'
            : 'bg-red-950/50 text-red-300 border-red-800/40'
        }`}
      >
        {report.status}
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
        <div class="text-lg font-bold text-cyan-300">{report.summary.sourceReferences.length}</div>
        <div class="text-[10px] text-cortex-400">{$t('agentReport.sourceRefs')}</div>
      </div>
      <div class="bg-cortex-800/30 rounded-lg p-2">
        <div class="text-lg font-bold text-amber-300">
          {report.summary.escalatedSegments.length}
        </div>
        <div class="text-[10px] text-cortex-400">{$t('agentReport.reviewQueue')}</div>
      </div>
    </div>

    <div class="space-y-1 text-[10px] text-cortex-400">
      <div class="flex justify-between gap-2">
        <span>{$t('agentReport.referenceModels')}</span>
        <span
          class="text-cortex-200 text-end truncate"
          title={report.summary.sourceReferenceModels.join(', ')}
        >
          {compactModels(report.summary.sourceReferenceModels)}
        </span>
      </div>
      <div class="flex justify-between gap-2">
        <span>{$t('agentReport.requiredReferenceModels')}</span>
        <span
          class="text-cortex-200 text-end truncate"
          title={report.summary.requiredSourceReferenceModels.join(', ')}
        >
          {compactModels(report.summary.requiredSourceReferenceModels)}
        </span>
      </div>
      <div class="flex justify-between gap-2">
        <span>{$t('agentReport.hypothesisModels')}</span>
        <span
          class="text-cortex-200 text-end truncate"
          title={report.summary.hypothesisModels.join(', ')}
        >
          {compactModels(report.summary.hypothesisModels)}
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
            {report.summary.agenticReadiness.status}
          </span>
        </div>
        <div class="space-y-1 text-[10px] text-cortex-400">
          <div class="flex justify-between gap-2">
            <span>{$t('agentReport.referenceModels')}</span>
            <span
              class="text-cortex-200 text-end truncate"
              title={report.summary.agenticReadiness.sourceReferenceModels.join(', ')}
            >
              {compactModels(report.summary.agenticReadiness.sourceReferenceModels)}
            </span>
          </div>
          <div class="flex justify-between gap-2">
            <span>{$t('agentReport.readyHypothesisModels')}</span>
            <span
              class="text-cortex-200 text-end truncate"
              title={report.summary.agenticReadiness.availableHypothesisModels.join(', ')}
            >
              {compactModels(report.summary.agenticReadiness.availableHypothesisModels)}
            </span>
          </div>
          <div class="flex justify-between gap-2">
            <span>{$t('agentReport.requiredHypothesisCount')}</span>
            <span class="text-cortex-200 font-mono">
              {report.summary.agenticReadiness.availableHypothesisModels.length}/{report.summary
                .agenticReadiness.requiredHypothesisModels}
            </span>
          </div>
        </div>
        <div class="space-y-1">
          {#each report.summary.agenticReadiness.checks.slice(0, 4) as check}
            <div class="space-y-0.5 text-[10px]" title={check.detail}>
              <div class="grid grid-cols-[minmax(0,1fr)_auto] gap-2">
                <span class="text-cortex-300 truncate">{check.label}</span>
                <span
                  class={`font-mono border rounded px-1 py-0.5 shrink-0 ${stageTone(check.status)}`}
                >
                  {check.status}
                </span>
              </div>
              <div class="text-cortex-500 truncate">{check.detail}</div>
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
            <span class="text-cortex-300 font-mono truncate">{model}</span>
            <span class="text-cortex-200 shrink-0">
              {count}/{report.summary.totalSegments}
            </span>
          </div>
        {/each}
      </div>
    {/if}

    {#if sourceReferenceCoverage().length}
      <div
        class="rounded bg-cortex-900/40 border border-cortex-800/40 p-2 space-y-1"
        data-testid="agent-report-source-reference-coverage"
      >
        <div class="text-[10px] text-cortex-500 uppercase tracking-wider">
          {$t('agentReport.referenceCoverage')}
        </div>
        {#each sourceReferenceCoverage().slice(0, 4) as coverage}
          <div
            class="grid grid-cols-[minmax(0,1fr)_auto] gap-2 text-[10px]"
            title={coverage.audioPath}
          >
            <span class="text-cortex-300 truncate">{shortPath(coverage.audioPath)}</span>
            <span
              class={`font-mono border rounded px-1 py-0.5 shrink-0 ${
                coverage.complete
                  ? 'text-emerald-300 bg-emerald-950/30 border-emerald-800/40'
                  : 'text-red-300 bg-red-950/30 border-red-800/40'
              }`}
              title={coverage.missingModels.join(', ')}
            >
              {coverage.presentModels.length}/{coverage.requiredModels.length ||
                coverage.presentModels.length}
              {#if !coverage.complete}
                {$t('agentReport.missing')}
              {/if}
            </span>
          </div>
        {/each}
        {#if sourceReferenceCoverage().length > 4}
          <div class="text-[10px] text-cortex-500 text-end">
            {$t('agentReport.more', { count: String(sourceReferenceCoverage().length - 4) })}
          </div>
        {/if}
      </div>
    {/if}

    {#if longFileDossiers().length}
      <div
        class="rounded bg-cortex-900/40 border border-cortex-800/40 p-2 space-y-1"
        data-testid="agent-report-long-file-dossiers"
      >
        <div class="text-[10px] text-cortex-500 uppercase tracking-wider">
          {$t('agentReport.longFileDossiers')}
        </div>
        {#each longFileDossiers().slice(0, 3) as dossier}
          <div class="space-y-1 text-[10px]" title={dossier.audioPath}>
            <div class="grid grid-cols-[minmax(0,1fr)_auto] gap-2">
              <span class="text-cortex-300 truncate">{shortPath(dossier.audioPath)}</span>
              <span
                class={`font-mono border rounded px-1 py-0.5 shrink-0 ${stageTone(dossier.promotionStatus)}`}
              >
                {dossier.promotionStatus}
              </span>
            </div>
            <div class="flex justify-between gap-2 text-cortex-500">
              <span>
                {dossier.chunkCount}
                {$t('agentReport.chunks')} - {dossier.trainingReadySegments}
                {$t('agentReport.readyShort')}
              </span>
              <span class="truncate text-end" title={dossier.promotionBlockers.join(', ')}>
                {#if dossier.promotionBlockers.length}
                  {dossier.promotionBlockers.slice(0, 2).join(', ')}
                {:else}
                  {$t('agentReport.noBlockers')}
                {/if}
              </span>
            </div>
          </div>
        {/each}
        {#if longFileDossiers().length > 3}
          <div class="text-[10px] text-cortex-500 text-end">
            {$t('agentReport.more', { count: String(longFileDossiers().length - 3) })}
          </div>
        {/if}
      </div>
    {/if}

    {#if recentStageEvents().length}
      <div
        class="rounded bg-cortex-900/40 border border-cortex-800/40 p-2 space-y-1"
        data-testid="agent-report-persisted-stage-events"
      >
        <div class="text-[10px] text-cortex-500 uppercase tracking-wider">
          {$t('agentReport.persistedStages')}
        </div>
        {#each recentStageEvents().slice(-6) as event}
          <div class="space-y-0.5 text-[10px]" title={`${event.file}: ${event.detail}`}>
            <div class="grid grid-cols-[minmax(0,1fr)_auto] gap-2">
              <span class="text-cortex-300 truncate">{event.stage}</span>
              <span
                class={`font-mono border rounded px-1 py-0.5 shrink-0 ${stageTone(event.status)}`}
              >
                {event.status}
                {#if event.total}
                  {event.current}/{event.total}
                {/if}
              </span>
            </div>
            <div class="text-cortex-500 truncate">{event.detail}</div>
          </div>
        {/each}
      </div>
    {/if}

    {#if report.summary.sourceReferences.length}
      <div
        class="rounded bg-cortex-900/40 border border-cortex-800/40 p-2 space-y-1"
        data-testid="agent-report-source-files"
      >
        <div class="text-[10px] text-cortex-500 uppercase tracking-wider">
          {$t('agentReport.sourceFiles')}
        </div>
        {#each report.summary.sourceReferences.slice(0, 3) as reference}
          <div
            class="grid grid-cols-[auto_minmax(0,1fr)] gap-x-2 gap-y-0.5 text-[10px]"
            title={`${reference.audioPath} | ${reference.transcriptPath} | ${sourceReferenceIdentity(reference)}`}
          >
            <span class="text-cyan-300 font-mono shrink-0 row-span-2">{reference.modelId}</span>
            <span class="text-cortex-300 text-end truncate min-w-0">
              {shortPath(reference.transcriptPath)} - {reference.textChars}
              {$t('agentReport.chars')}
            </span>
            {#if sourceReferenceIdentity(reference)}
              <span class="text-cortex-500 text-end truncate min-w-0">
                {sourceReferenceIdentity(reference)}
              </span>
            {/if}
          </div>
        {/each}
        {#if report.summary.sourceReferences.length > 3}
          <div class="text-[10px] text-cortex-500 text-end">
            {$t('agentReport.more', { count: String(report.summary.sourceReferences.length - 3) })}
          </div>
        {/if}
      </div>
    {/if}

    {#if report.summary.escalatedSegments.length}
      <div
        class="rounded bg-amber-950/20 border border-amber-900/30 p-2 space-y-1"
        data-testid="agent-report-escalated-ids"
      >
        <div class="text-[10px] text-amber-300 uppercase tracking-wider">
          {$t('agentReport.escalatedIds')}
        </div>
        <div class="text-[10px] text-cortex-300 font-mono break-all">
          {report.summary.escalatedSegments.slice(0, 6).join(', ')}
          {#if report.summary.escalatedSegments.length > 6}
            {$t('agentReport.more', { count: String(report.summary.escalatedSegments.length - 6) })}
          {/if}
        </div>
      </div>
    {/if}

    {#if orchestrationStages().length}
      <div
        class="rounded bg-cortex-900/40 border border-cortex-800/40 p-2 space-y-1"
        data-testid="agent-report-orchestration-stages"
      >
        <div class="text-[10px] text-cortex-500 uppercase tracking-wider">
          {$t('agentReport.orchestrationStages')}
        </div>
        {#each orchestrationStages().slice(0, 5) as stage}
          <div class="grid grid-cols-[minmax(0,1fr)_auto] gap-2 text-[10px]" title={stage.summary}>
            <span class="text-cortex-300 truncate">{stage.stage}</span>
            <span
              class={`font-mono border rounded px-1 py-0.5 shrink-0 ${stageTone(stage.status)}`}
            >
              {stage.status}
              {#if stage.blockerCount}
                - {stage.blockerCount}
              {/if}
            </span>
          </div>
        {/each}
      </div>
    {/if}

    {#if coverageBlockers().length}
      <div
        class="rounded bg-amber-950/20 border border-amber-900/30 p-2 space-y-1"
        data-testid="agent-report-coverage-blockers"
      >
        <div class="flex justify-between gap-2 text-[10px] text-amber-300 uppercase tracking-wider">
          <span>{$t('agentReport.coverageBlockers')}</span>
          <span class="font-mono">{coverageBlockers().length}</span>
        </div>
        {#each coverageBlockers().slice(0, 4) as blocker}
          <div class="flex justify-between gap-2 text-[10px]">
            <span class="text-cortex-300 font-mono truncate" title={blocker.segmentId}
              >{blocker.segmentId}</span
            >
            <span class="text-cortex-200 shrink-0">
              {blocker.coverage.nonEmptyModelCount}/{blocker.coverage.minimumNonEmptyModelCount}
            </span>
          </div>
        {/each}
        {#if coverageBlockers().length > 4}
          <div class="text-[10px] text-cortex-500 text-end">
            {$t('agentReport.more', { count: String(coverageBlockers().length - 4) })}
          </div>
        {/if}
      </div>
    {/if}

    {#if topCounts(report.summary.trainingGradeReasonCounts, 4).length}
      <div
        class="rounded bg-cortex-900/40 border border-cortex-800/40 p-2 space-y-1"
        data-testid="agent-report-grade-reasons"
      >
        <div class="text-[10px] text-cortex-500 uppercase tracking-wider">
          {$t('agentReport.gradeReasons')}
        </div>
        {#each topCounts(report.summary.trainingGradeReasonCounts, 4) as [reason, count]}
          <div class="flex justify-between gap-2 text-[10px]">
            <span class="text-cortex-300 truncate" title={reason}>{reason}</span>
            <span class="text-cortex-200 font-mono shrink-0">{count}</span>
          </div>
        {/each}
      </div>
    {/if}

    <div class="grid grid-cols-4 gap-1 text-center">
      <div class="rounded bg-cortex-900/50 border border-cortex-800/30 px-1.5 py-2">
        <div class="text-xs font-semibold text-emerald-300">{countFor('jury_accept')}</div>
        <div class="text-[9px] text-cortex-500">jury</div>
      </div>
      <div class="rounded bg-cortex-900/50 border border-cortex-800/30 px-1.5 py-2">
        <div class="text-xs font-semibold text-cyan-300">{countFor('auto_accept')}</div>
        <div class="text-[9px] text-cortex-500">auto</div>
      </div>
      <div class="rounded bg-cortex-900/50 border border-cortex-800/30 px-1.5 py-2">
        <div class="text-xs font-semibold text-amber-300">{countFor('escalated')}</div>
        <div class="text-[9px] text-cortex-500">review</div>
      </div>
      <div class="rounded bg-cortex-900/50 border border-cortex-800/30 px-1.5 py-2">
        <div class="text-xs font-semibold text-cortex-300">{countFor('unprocessed')}</div>
        <div class="text-[9px] text-cortex-500">open</div>
      </div>
    </div>

    {#if report.error}
      <div class="text-[10px] text-red-300 bg-red-950/30 border border-red-900/40 rounded p-2">
        {report.error}
      </div>
    {/if}
  </section>
{/if}
