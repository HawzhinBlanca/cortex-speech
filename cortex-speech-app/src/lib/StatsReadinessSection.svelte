<script lang="ts">
  import type { AudioHealth, TrainingGradeBreakdown } from './commands';
  import { t } from './i18n';
  import {
    formatDuration,
    formatPercent,
    formatRatePercent,
    type AccuracyRecord,
    type StatsBlocker,
  } from './statsDashboardModel';
  import type { DatasetStats } from './types';

  let {
    stats,
    audioHealth,
    breakdown,
    blockers,
    verdict,
    accuracy,
    evalRunsLoaded,
    fingerprintCount,
    relinking,
    onRelink,
    onOpenReview,
  }: {
    stats: DatasetStats;
    audioHealth: AudioHealth | null;
    breakdown: TrainingGradeBreakdown | null;
    blockers: StatsBlocker[];
    verdict: 'ready' | 'notReady' | 'unknown';
    accuracy: AccuracyRecord | null;
    evalRunsLoaded: boolean;
    fingerprintCount: number | null;
    relinking: boolean;
    onRelink: () => void;
    onOpenReview?: () => void;
  } = $props();

  const nextAction = $derived(blockers[0] ?? null);

  function blockerText(blocker: StatsBlocker): string {
    const n = String(blocker.count ?? 0);
    switch (blocker.id) {
      case 'audioMissing':
        return $t('stats.blockerAudioMissing', { n });
      case 'pendingReview':
        return $t('stats.blockerPendingReview', { n });
      case 'emptyTranscripts':
        return $t('stats.blockerEmptyTranscripts', { n });
      case 'qualityGate':
        return $t('stats.blockerQualityGate', { n });
      case 'nothingTrainingReady':
        return $t('stats.blockerNothingReady', { n });
      case 'noAccuracyRecord':
        return $t('stats.blockerNoAccuracyRecord');
      default:
        return blocker.id;
    }
  }
</script>

{#if audioHealth && audioHealth.missingFiles > 0}
  <div
    class="mb-3 flex items-center justify-between gap-3 rounded-lg border border-amber-600/40 bg-amber-950/30 p-3"
    data-testid="audio-missing-banner"
  >
    <span class="text-sm text-amber-300">
      {$t('stats.audioMissing').replace('{n}', String(audioHealth.missingFiles))}
    </span>
    <button
      type="button"
      class="btn btn-primary !text-xs"
      data-testid="relink-audio-btn"
      disabled={relinking}
      onclick={onRelink}
    >
      {relinking ? $t('stats.relinking') : $t('stats.relink')}
    </button>
  </div>
{/if}

<div class="space-y-3" data-testid="readiness-verdict">
  <div
    class="rounded-lg border p-3 {verdict === 'ready'
      ? 'border-emerald-600/40 bg-emerald-950/20'
      : verdict === 'notReady'
        ? 'border-amber-600/40 bg-amber-950/20'
        : 'border-cortex-700/40 bg-cortex-900/30'}"
  >
    <div class="flex items-baseline justify-between gap-3">
      <span
        class="text-lg font-bold {verdict === 'ready'
          ? 'text-emerald-300'
          : verdict === 'notReady'
            ? 'text-amber-300'
            : 'text-cortex-400'}"
        data-testid="readiness-headline"
      >
        {verdict === 'ready'
          ? $t('stats.ready')
          : verdict === 'notReady'
            ? $t('stats.notReady')
            : $t('stats.readinessUnknown')}
      </span>
      {#if breakdown}
        <span class="text-xs text-cortex-400" data-testid="readiness-count">
          <bdi dir="ltr">
            {breakdown.summary.trainingReadySegments}/{breakdown.summary.totalSegments}
          </bdi>
        </span>
      {/if}
    </div>
    <p class="mt-1 text-[11px] text-cortex-400 leading-tight">{$t('stats.readyExplain')}</p>
  </div>

  {#if blockers.length > 0}
    <div class="space-y-1.5" data-testid="readiness-blockers">
      <span class="text-xs text-cortex-400 uppercase tracking-wider">{$t('stats.blockers')}</span>
      {#each blockers as blocker (blocker.id)}
        <div class="flex items-center justify-between gap-2">
          <span class="text-xs text-cortex-200">
            {blockerText(blocker)}
            {#if blocker.detail}
              <bdi class="text-[10px] font-mono text-cortex-500">{blocker.detail}</bdi>
            {/if}
          </span>
          {#if blocker.action === 'relink'}
            <button
              type="button"
              class="btn btn-secondary !text-[11px] shrink-0"
              data-testid="blocker-relink-btn"
              disabled={relinking}
              onclick={onRelink}
            >
              {relinking ? $t('stats.relinking') : $t('stats.relink')}
            </button>
          {:else if blocker.action === 'review' && onOpenReview}
            <button
              type="button"
              class="btn btn-secondary !text-[11px] shrink-0"
              data-testid="blocker-review-btn"
              onclick={onOpenReview}
            >
              {$t('stats.blockerOpenReview')}
            </button>
          {/if}
        </div>
      {/each}
      {#if nextAction}
        <p class="text-[11px] text-cortex-300 pt-1" data-testid="readiness-next-action">
          <span class="text-cortex-500">{$t('stats.nextAction')}</span>
          {blockerText(nextAction)}
        </p>
      {/if}
    </div>
  {/if}

  {#if accuracy}
    <div
      class="rounded-lg border border-cortex-700/40 bg-cortex-900/30 p-3 space-y-1"
      data-testid="accuracy-record"
    >
      <span class="text-xs text-cortex-400 uppercase tracking-wider">
        {$t('stats.accuracyTitle')}
      </span>
      <div class="flex flex-wrap items-baseline gap-x-4 gap-y-1">
        <span class="text-sm">
          <span class="text-cortex-500 text-[11px]">{$t('stats.cerLabel')}</span>
          <bdi dir="ltr" class="font-bold text-cortex-100"
            >{formatRatePercent(accuracy.run.cer)}</bdi
          >
          {#if accuracy.cerLow !== null && accuracy.cerHigh !== null}
            <bdi dir="ltr" class="text-[10px] text-cortex-500">
              [{formatRatePercent(accuracy.cerLow)}–{formatRatePercent(accuracy.cerHigh)}]
            </bdi>
          {/if}
        </span>
        <span class="text-sm">
          <span class="text-cortex-500 text-[11px]">{$t('stats.werLabel')}</span>
          <bdi dir="ltr" class="font-bold text-cortex-100"
            >{formatRatePercent(accuracy.run.wer)}</bdi
          >
          {#if accuracy.werLow !== null && accuracy.werHigh !== null}
            <bdi dir="ltr" class="text-[10px] text-cortex-500">
              [{formatRatePercent(accuracy.werLow)}–{formatRatePercent(accuracy.werHigh)}]
            </bdi>
          {/if}
        </span>
      </div>
      <div class="text-[10px] text-cortex-500 font-mono break-all">
        <bdi dir="ltr">
          {accuracy.run.modelId} · N={accuracy.run.numSegs} · {accuracy.run.runAt} · {accuracy.run.id.slice(
            0,
            8,
          )}
        </bdi>
      </div>
    </div>
  {:else if evalRunsLoaded}
    <p class="text-[11px] text-amber-400/90" data-testid="accuracy-none">
      {$t('stats.accuracyNone')}
    </p>
  {/if}
</div>

<div class="grid grid-cols-2 gap-3">
  <div class="bg-cortex-800/30 rounded-lg p-3">
    <div class="text-2xl font-bold text-cortex-200">{stats.totalSegments}</div>
    <div class="text-xs text-cortex-400">{$t('stats.totalSegments')}</div>
  </div>
  <div class="bg-cortex-800/30 rounded-lg p-3">
    <div class="text-2xl font-bold text-emerald-400">
      {formatDuration(stats.totalDurationSeconds)}
    </div>
    <div class="text-xs text-cortex-400">{$t('stats.totalDuration')}</div>
  </div>
  <div class="bg-cortex-800/30 rounded-lg p-3">
    <div class="text-2xl font-bold text-amber-400">{formatPercent(stats.verificationRate)}</div>
    <div class="text-xs text-cortex-400">
      {$t('stats.verified')} ({stats.verifiedCount}/{stats.totalSegments})
    </div>
  </div>
  <div class="bg-cortex-800/30 rounded-lg p-3">
    <div class="text-2xl font-bold text-cortex-300">{stats.uniqueSpeakers}</div>
    <div class="text-xs text-cortex-400">{$t('stats.uniqueSpeakers')}</div>
  </div>
  {#if stats.reviewTiming && stats.reviewTiming.medianSeconds !== null}
    <div class="bg-cortex-800/30 rounded-lg p-3" data-testid="stat-review-speed">
      <div class="text-2xl font-bold text-cortex-300">
        {stats.reviewTiming.medianSeconds.toFixed(1)}s
      </div>
      <div class="text-xs text-cortex-400">
        {$t('stats.reviewSpeed')} ({stats.reviewTiming.samples})
      </div>
    </div>
  {/if}
  {#if stats.dbSizeBytes > 0}
    <div class="bg-cortex-800/30 rounded-lg p-3" data-testid="stat-db-size">
      <div class="text-2xl font-bold text-cortex-300">
        {(stats.dbSizeBytes / 1048576).toFixed(1)} MB
      </div>
      <div class="text-xs text-cortex-400">{$t('stats.dbSize')}</div>
    </div>
  {/if}
  {#if fingerprintCount !== null}
    <div class="bg-cortex-800/30 rounded-lg p-3" data-testid="stat-fingerprints">
      <div class="text-2xl font-bold text-cortex-300">{fingerprintCount}</div>
      <div class="text-xs text-cortex-400">{$t('stats.fingerprints')}</div>
    </div>
  {/if}
</div>
