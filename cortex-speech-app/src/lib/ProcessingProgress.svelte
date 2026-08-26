<script lang="ts">
  // Prominent processing indicator shown as a banner under the top toolbar during any pipeline run.
  // Replaces "the progress is a 10px line in the status bar" with a real bar + percent + elapsed +
  // ETA + always-visible stage chips. Reads the existing pipeline stores; the math lives in the
  // unit-tested progressStats.ts. Frontend-only — no backend/IPC change.
  import LoaderCircle from '@lucide/svelte/icons/loader-circle';
  import { t } from './i18n';
  import { cancelOperation } from './commands';
  import {
    pipelinePhase,
    filesProcessed,
    pipelineTotal,
    pipelineCurrentFile,
    pipelineStatus,
    agentPipelineStages,
    batchProgress,
    isProcessing,
    type AgentPipelineStage,
  } from './stores/uiStore';
  import { computeProgress, formatDurationShort } from './progressStats';

  // Active for any pipeline phase, a running batch-verify, or a generic processing flag.
  const active = $derived(
    $pipelinePhase !== 'idle' || $batchProgress.status === 'running' || $isProcessing,
  );

  // Elapsed timer: starts on the first active tick, ticks each second, resets when idle. new Date()
  // is fine here (browser runtime); the pure ETA math is tested separately.
  let startedAt = $state<number | null>(null);
  let nowMs = $state(0);
  $effect(() => {
    if (!active) {
      startedAt = null;
      return;
    }
    if (startedAt === null) {
      startedAt = Date.now();
      nowMs = startedAt;
    }
    const id = setInterval(() => {
      nowMs = Date.now();
    }, 1000);
    return () => clearInterval(id);
  });
  const elapsedMs = $derived(startedAt !== null ? Math.max(0, nowMs - startedAt) : 0);

  // Prefer the pipeline counters; fall back to the batch-verify counters.
  const current = $derived($filesProcessed || $batchProgress.completed || 0);
  const total = $derived($pipelineTotal || $batchProgress.total || 0);

  // ETA baseline: the ETA extrapolates elapsed/done*(remaining), so its `elapsed` must measure ONLY the
  // counted (done/total) scope — NOT any earlier phase. For a single-file import the slow whole-file
  // `reference_transcribing` pass runs (and accrues into elapsedMs) BEFORE per-chunk Progress starts, so
  // feeding the whole elapsed made the first chunk's ETA ≈ (reference seconds) × remaining chunks — wildly
  // inflated. Reset the baseline when the counted scope (total>0) begins so the ETA is chunk-scoped; the
  // DISPLAYED elapsed (row 3) stays whole-run.
  let etaBaselineMs = $state<number | null>(null);
  $effect(() => {
    if (!active || total <= 0) {
      if (etaBaselineMs !== null) etaBaselineMs = null;
    } else if (etaBaselineMs === null) {
      etaBaselineMs = Date.now();
    }
  });
  const etaElapsedMs = $derived(etaBaselineMs !== null ? Math.max(0, nowMs - etaBaselineMs) : 0);
  const stats = $derived(computeProgress(current, total, etaElapsedMs));

  const phaseLabel = $derived.by(() => {
    switch ($pipelinePhase) {
      case 'importing':
        return $t('pipeline.importing');
      case 'reference_transcribing':
        return $t('pipeline.referenceTranscribing');
      case 'detecting':
        return $t('pipeline.detecting');
      case 'transcribing':
        return $t('pipeline.transcribing');
      case 'adjudicating':
        return $t('pipeline.adjudicating');
      default:
        return $t('processing');
    }
  });

  const fileName = $derived(
    $pipelineCurrentFile ? ($pipelineCurrentFile.split(/[/\\]/).pop() ?? $pipelineCurrentFile) : '',
  );

  // Dedupe by stage name (keep the latest entry per stage) so the keyed {#each} below always has
  // UNIQUE keys. agentPipelineStages is populated two ways: upsertAgentPipelineStage (already unique)
  // AND a raw .set() from DB history (App.svelte loadLatestAgentStageEvents) that does NOT dedupe —
  // that path can repeat a stage name, and a keyed each with a duplicate key throws each_key_duplicate.
  const recentStages = $derived.by(() => {
    const byStage = new Map<string, AgentPipelineStage>();
    for (const s of $agentPipelineStages) byStage.set(s.stage, s);
    return Array.from(byStage.values()).slice(-6);
  });

  function stageTone(status: string): string {
    if (status === 'completed') return 'border-emerald-700/40 text-emerald-300 bg-emerald-950/30';
    if (status === 'blocked') return 'border-red-700/40 text-red-300 bg-red-950/30';
    return 'border-amber-700/40 text-amber-300 bg-amber-950/30';
  }
  const stageLabel = (s: string) => s.replaceAll('_', ' ');
</script>

{#if active}
  <div
    data-testid="processing-progress"
    class="shrink-0 px-4 py-2.5 glass border-b border-line"
    role="progressbar"
    aria-valuemin="0"
    aria-valuemax="100"
    aria-valuenow={stats.hasPercent ? stats.percent : undefined}
    aria-label={phaseLabel}
  >
    <!-- Row 1: phase + live detail + percent + cancel -->
    <div class="flex items-center gap-3">
      <LoaderCircle class="h-4 w-4 shrink-0 animate-spin text-amber-400" aria-hidden="true" />
      <span class="text-sm font-semibold text-default shrink-0">{phaseLabel}</span>
      {#if $pipelineStatus}
        <span class="text-xs text-cortex-400 truncate" title={$pipelineStatus}
          >{$pipelineStatus}</span
        >
      {/if}
      <span class="flex-1"></span>
      {#if stats.hasPercent}
        <span
          data-testid="processing-percent"
          class="text-sm font-mono font-semibold text-amber-300 tabular-nums"
        >
          {stats.percent}%
        </span>
      {/if}
      <button
        class="text-red-400 hover:text-red-300 text-xs px-2 py-0.5 border border-red-500/40 rounded shrink-0"
        data-testid="processing-cancel"
        onclick={cancelOperation}>{$t('pipeline.cancel')}</button
      >
    </div>

    <!-- Row 2: the bar (determinate percent, or an indeterminate sweep when the total is unknown) -->
    <div class="mt-2 h-2 w-full bg-cortex-700/60 rounded-full overflow-hidden">
      {#if stats.hasPercent}
        <div
          class="h-full bg-amber-400 rounded-full transition-[width] duration-500 ease-out"
          style="width: {stats.percent}%"
        ></div>
      {:else}
        <div class="h-full w-1/3 bg-amber-400/80 rounded-full indeterminate"></div>
      {/if}
    </div>

    <!-- Row 3: counts, elapsed, ETA, file, stage chips -->
    <div class="mt-1.5 flex items-center flex-wrap gap-x-4 gap-y-1 text-[11px] text-cortex-400">
      {#if stats.total > 0}
        <span class="tabular-nums"
          >{$t('pipeline.progressCount', {
            done: String(stats.done),
            total: String(stats.total),
          })}</span
        >
      {/if}
      <span class="tabular-nums"
        >{$t('pipeline.elapsed', { time: formatDurationShort(elapsedMs) })}</span
      >
      {#if stats.etaMs !== null && stats.etaMs > 0}
        <span class="tabular-nums text-cortex-500"
          >{$t('pipeline.eta', {
            time: formatDurationShort(stats.etaMs),
          })}</span
        >
      {/if}
      {#if fileName}
        <span class="truncate max-w-[16rem] text-cortex-500" title={$pipelineCurrentFile}
          >{fileName}</span
        >
      {/if}
      {#if recentStages.length}
        <span class="flex items-center gap-1 flex-wrap">
          {#each recentStages as stage (stage.stage)}
            <span
              class={`px-1.5 py-0.5 rounded border font-mono text-[10px] ${stageTone(stage.status)}`}
              title={`${stageLabel(stage.stage)}: ${stage.detail}`}
            >
              {stageLabel(stage.stage)}:{stage.status}
            </span>
          {/each}
        </span>
      {/if}
    </div>
  </div>
{/if}

<style>
  /* Indeterminate sweep for phases with no known total (e.g. building the reference transcript). */
  .indeterminate {
    animation: indeterminate-slide 1.3s ease-in-out infinite;
  }
  @keyframes indeterminate-slide {
    0% {
      transform: translateX(-120%);
    }
    100% {
      transform: translateX(320%);
    }
  }
</style>
