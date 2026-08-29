<script lang="ts">
  import type { ConformalCertificate } from './commands';
  import { t } from './i18n';
  import { formatDuration } from './statsDashboardModel';
  import type { DatasetStats } from './types';

  let {
    stats,
    durationBuckets,
    maxBucket,
    cert,
  }: {
    stats: DatasetStats;
    durationBuckets: { label: string; value: number }[];
    maxBucket: number;
    cert: ConformalCertificate | null;
  } = $props();
</script>

<div class="space-y-1">
  <span class="text-xs text-cortex-400">{$t('stats.durationDistribution')}</span>
  <div class="flex gap-1 h-16 items-end">
    {#each durationBuckets as bar}
      {#if stats.totalSegments > 0}
        <div class="flex-1 flex flex-col items-center gap-1">
          <div
            class="w-full bg-cortex-600 rounded-t transition-all duration-500"
            style="height: {(bar.value / maxBucket) * 100}%"
            title="{bar.value} ({stats.totalSegments > 0
              ? ((bar.value / stats.totalSegments) * 100).toFixed(0)
              : 0}%)"
          ></div>
          <span class="text-[10px] text-cortex-400">{bar.label}</span>
        </div>
      {/if}
    {/each}
  </div>
</div>

{#if stats.topSpeakers.length > 0}
  <div class="space-y-1">
    <span class="text-xs text-cortex-400">{$t('stats.topSpeakers')}</span>
    <div class="space-y-1 max-h-32 overflow-y-auto">
      {#each stats.topSpeakers as speaker, index}
        <div class="flex items-center gap-2 text-xs">
          <span class="text-cortex-500 w-4">{index + 1}.</span>
          <span class="text-cortex-200 flex-1 truncate">{speaker.speakerId}</span>
          <span class="text-cortex-400">{speaker.segmentCount} {$t('stats.segShort')}</span>
          <span class="text-cortex-500">{formatDuration(speaker.totalDurationSeconds)}</span>
        </div>
      {/each}
    </div>
  </div>
{/if}

{#if cert}
  <div class="space-y-2 pt-2 border-t border-cortex-800/50" data-testid="conformal-cert">
    <h3
      class="text-xs font-semibold text-cortex-300 uppercase tracking-wider flex items-center justify-between"
    >
      <span>{$t('stats.conformalTitle')}</span>
      {#if cert.isCalibrated}
        <span
          class="text-[9px] px-1.5 py-0.5 rounded bg-emerald-950/50 text-emerald-400 border border-emerald-800/40 font-mono"
          >{$t('stats.calibrated')}</span
        >
      {:else}
        <span
          class="text-[9px] px-1.5 py-0.5 rounded bg-amber-950/50 text-amber-400 border border-amber-800/40 font-mono"
          >{$t('stats.heuristic')}</span
        >
      {/if}
    </h3>

    <div class="bg-cortex-900/40 border border-cortex-800/40 rounded-xl p-3 space-y-2">
      <div class="grid grid-cols-2 gap-2 text-center">
        <div class="bg-cortex-950/40 p-2 rounded-lg border border-cortex-800/20">
          <div class="text-lg font-bold" class:text-cyan-400={cert.isCalibrated}>
            {cert.isCalibrated ? cert.totalCertified : '—'}
          </div>
          <div class="text-[9px] text-cortex-400">{$t('stats.certifiedSegments')}</div>
        </div>
        <div class="bg-cortex-950/40 p-2 rounded-lg border border-cortex-800/20">
          <div class="text-lg font-bold text-cortex-200">
            {Number.isFinite(cert.threshold) ? cert.threshold.toFixed(3) : '—'}
          </div>
          <div class="text-[9px] text-cortex-400">{$t('stats.decisionThreshold')}</div>
        </div>
      </div>

      <div class="text-[10px] text-cortex-400 space-y-1">
        <div class="flex justify-between">
          <span>{$t('stats.targetErrorBound')}</span>
          <span class="font-semibold text-cortex-200">{(cert.targetError * 100).toFixed(0)}%</span>
        </div>
        <div class="flex justify-between">
          <span>{$t('stats.confidenceLevel')}</span>
          <span class="font-semibold text-cortex-200"
            >{(cert.confidenceLevel * 100).toFixed(0)}%</span
          >
        </div>
        <div class="flex justify-between">
          <span>{$t('stats.expectedErrorBound')}</span>
          {#if cert.isCalibrated}
            <span class="font-semibold text-emerald-400"
              >{(cert.expectedErrorBound * 100).toFixed(1)}%</span
            >
          {:else}
            <span
              class="font-semibold text-amber-400/90"
              title={$t('stats.uncalibratedTargetTitle')}>{$t('stats.uncalibratedValue')}</span
            >
          {/if}
        </div>
      </div>

      {#if cert.calibrationNoConfidence > 0 && cert.calibrationRealPosterior + cert.calibrationHeuristic === 0}
        <p class="text-[9px] text-amber-400/90 leading-tight" data-testid="conformal-no-confidence">
          {$t('stats.conformalNoConfidence')}
        </p>
      {:else if !cert.isCalibrated}
        <p class="text-[9px] text-amber-400/90 leading-tight">
          {$t('stats.conformalUncalibrated')}
        </p>
      {/if}

      {#if cert.calibrationRealPosterior === 0 && cert.calibrationHeuristic > 0}
        <p
          class="text-[9px] text-amber-400/90 leading-tight"
          data-testid="conformal-heuristic-basis"
        >
          {$t('stats.conformalHeuristicBasis')}
        </p>
      {/if}
    </div>
  </div>
{/if}
