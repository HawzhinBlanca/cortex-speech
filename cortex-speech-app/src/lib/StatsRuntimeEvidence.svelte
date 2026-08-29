<script lang="ts">
  import type { IntelligenceReport } from './commands';
  import { t } from './i18n';
  import { formatMilliseconds, type InferenceStats } from './statsDashboardModel';

  let {
    inferenceStats,
    intel,
  }: {
    inferenceStats: InferenceStats | null;
    intel: IntelligenceReport | null;
  } = $props();
</script>

{#if inferenceStats}
  <div class="space-y-2">
    <h3 class="text-xs font-semibold text-cortex-300 uppercase tracking-wider">
      {$t('stats.inferenceTitle')}
    </h3>
    <div class="grid grid-cols-2 gap-2">
      <div class="bg-cortex-800/30 rounded-lg p-2">
        <div class="text-[10px] text-cortex-400 mb-1">{$t('inference.vad')}</div>
        <div class="text-sm font-bold text-cortex-200">
          {inferenceStats.vad.calls}
          {$t('inference.calls')}
        </div>
        <div class="text-[10px] text-cortex-500">
          {inferenceStats.vad.failures}
          {$t('inference.failures')}
          {#if inferenceStats.vad.calls > 0}
            &middot; {((1 - inferenceStats.vad.failures / inferenceStats.vad.calls) * 100).toFixed(
              1,
            )}% ok
          {/if}
        </div>
        <div class="text-[10px] text-cortex-500 mt-0.5">
          {$t('inference.p50')}
          {formatMilliseconds(inferenceStats.vad.p50_ms)} &middot;
          {$t('inference.p99')}
          {formatMilliseconds(inferenceStats.vad.p99_ms)}
        </div>
      </div>

      <div class="bg-cortex-800/30 rounded-lg p-2">
        <div class="text-[10px] text-cortex-400 mb-1">{$t('inference.asr')}</div>
        <div class="text-sm font-bold text-cortex-200">
          {inferenceStats.asr.calls}
          {$t('inference.calls')}
        </div>
        <div class="text-[10px] text-cortex-500">
          {inferenceStats.asr.failures}
          {$t('inference.failures')}
          {#if inferenceStats.asr.calls > 0}
            &middot; {((1 - inferenceStats.asr.failures / inferenceStats.asr.calls) * 100).toFixed(
              1,
            )}% ok
          {/if}
        </div>
        <div class="text-[10px] text-cortex-500 mt-0.5">
          {$t('inference.p50')}
          {formatMilliseconds(inferenceStats.asr.p50_ms)} &middot;
          {$t('inference.p99')}
          {formatMilliseconds(inferenceStats.asr.p99_ms)}
        </div>
      </div>
    </div>
    {#if inferenceStats.model_load_ms > 0}
      <div class="text-[10px] text-cortex-500">
        {$t('inference.modelLoad')}: {formatMilliseconds(inferenceStats.model_load_ms)}
      </div>
    {/if}
  </div>
{/if}

{#if intel && (intel.loop0Shadow.totalObservations > 0 || intel.autoAcceptPrecision.t0Accepts + intel.autoAcceptPrecision.t1Escalations > 0)}
  <div class="space-y-2 pt-2 border-t border-cortex-800/50" data-testid="intelligence-report">
    <h3 class="text-xs font-semibold text-cortex-300 uppercase tracking-wider">
      {$t('stats.intelTitle')}
    </h3>
    <div class="grid grid-cols-2 gap-2">
      <div class="bg-cortex-800/30 rounded-lg p-2">
        <div
          class="text-sm font-bold {intel.loop0Shadow.wouldFire === 0
            ? 'text-cortex-400'
            : intel.loop0Shadow.firedButHumanAcceptedOriginal === 0
              ? 'text-emerald-300'
              : 'text-red-300'}"
          data-testid="loop0-overtriggers"
        >
          {intel.loop0Shadow.wouldFire === 0
            ? '—'
            : intel.loop0Shadow.firedButHumanAcceptedOriginal}
        </div>
        <div class="text-[10px] text-cortex-400">
          {$t('stats.loop0OverTriggers')} ({intel.loop0Shadow.wouldFire}/{intel.loop0Shadow
            .totalObservations}
          {$t('stats.loop0WouldFire')}{intel.loop0Shadow.wouldFire === 0
            ? ` · ${$t('stats.noEvidenceYet')}`
            : ''})
        </div>
      </div>
      <div class="bg-cortex-800/30 rounded-lg p-2">
        <div class="text-sm font-bold text-cortex-200" data-testid="c4-precision">
          {intel.autoAcceptPrecision.t0HumanConfirmed +
            intel.autoAcceptPrecision.t0HumanContradicted >
          0
            ? `${(
                (100 * intel.autoAcceptPrecision.t0HumanConfirmed) /
                (intel.autoAcceptPrecision.t0HumanConfirmed +
                  intel.autoAcceptPrecision.t0HumanContradicted)
              ).toFixed(0)}%`
            : '—'}
        </div>
        <div class="text-[10px] text-cortex-400">
          {$t('stats.c4Precision')} ({intel.autoAcceptPrecision.t0HumanConfirmed}/{intel
            .autoAcceptPrecision.t0HumanConfirmed + intel.autoAcceptPrecision.t0HumanContradicted}
          · T0 {intel.autoAcceptPrecision.t0Accepts} / T1 {intel.autoAcceptPrecision.t1Escalations})
        </div>
      </div>
    </div>
    {#if intel.conformalCalibration}
      <div class="bg-cortex-800/30 rounded-lg p-2" data-testid="conformal-progress">
        <div class="text-[10px] text-cortex-400 mb-1">
          {$t('stats.conformalProgress', {
            n: String(intel.conformalCalibration.minNeededAtZeroCer),
          })}
        </div>
        <div class="flex flex-wrap gap-x-3 gap-y-0.5">
          {#each intel.conformalCalibration.buckets as bucket (bucket.bucket)}
            <span class="text-[10px] text-cortex-500">
              <bdi>{bucket.bucket}</bdi>: {bucket.verifiedWithReference}/{bucket.minNeededAtZeroCer}
            </span>
          {/each}
        </div>
      </div>
    {/if}
  </div>
{/if}
