<script lang="ts">
  import type { DatasetQuality } from './commands';
  import { t } from './i18n';

  let { quality }: { quality: DatasetQuality | null } = $props();
</script>

{#if quality && quality.totalSegments > 0}
  <div class="space-y-2">
    <h3 class="text-xs font-semibold text-cortex-300 uppercase tracking-wider">
      {$t('stats.qualityTitle')}
    </h3>
    <div class="grid grid-cols-2 gap-2">
      <div class="bg-cortex-800/30 rounded-lg p-2">
        <div class="text-sm font-bold text-red-300">{quality.emptyTranscriptCount}</div>
        <div class="text-[10px] text-cortex-400">{$t('stats.emptyTranscripts')}</div>
      </div>
      <div class="bg-cortex-800/30 rounded-lg p-2">
        <div class="text-sm font-bold text-amber-300">{quality.lowConfidenceCount}</div>
        <div class="text-[10px] text-cortex-400">{$t('stats.lowConfidence')}</div>
      </div>
      <div class="bg-cortex-800/30 rounded-lg p-2">
        <div class="text-sm font-bold text-orange-300">{quality.duplicateTranscriptGroups}</div>
        <div class="text-[10px] text-cortex-400">
          {$t('stats.duplicateGroups')} ({quality.duplicateTranscriptSegments}
          {$t('stats.segShort')})
        </div>
      </div>
      <div class="bg-cortex-800/30 rounded-lg p-2">
        <div class="text-sm font-bold text-purple-300">{quality.durationOutlierCount}</div>
        <div class="text-[10px] text-cortex-400">{$t('stats.durationOutliers')}</div>
      </div>
    </div>
    {#if quality.annotatedSegmentCount > 0}
      <div class="grid grid-cols-2 gap-2">
        <div class="bg-cortex-800/30 rounded-lg p-2">
          <div
            class="text-sm font-bold {quality.segmentsAboveWerThreshold === 0
              ? 'text-emerald-300'
              : 'text-red-300'}"
          >
            {quality.meanWer != null ? `${(quality.meanWer * 100).toFixed(1)}%` : '—'}
          </div>
          <div class="text-[10px] text-cortex-400">
            {$t('stats.meanWer')} ({quality.annotatedSegmentCount}
            {$t('stats.annotated')})
          </div>
        </div>
        <div class="bg-cortex-800/30 rounded-lg p-2">
          <div
            class="text-sm font-bold {quality.segmentsAboveCerThreshold === 0
              ? 'text-emerald-300'
              : 'text-red-300'}"
          >
            {quality.meanCer != null ? `${(quality.meanCer * 100).toFixed(1)}%` : '—'}
          </div>
          <div class="text-[10px] text-cortex-400">{$t('stats.meanCer')}</div>
        </div>
        <div class="bg-cortex-800/30 rounded-lg p-2">
          <div class="text-sm font-bold text-orange-300">
            {quality.segmentsAboveWerThreshold}
          </div>
          <div class="text-[10px] text-cortex-400">{$t('stats.aboveWer')}</div>
        </div>
        <div class="bg-cortex-800/30 rounded-lg p-2">
          <div class="text-sm font-bold text-orange-300">
            {quality.segmentsAboveCerThreshold}
          </div>
          <div class="text-[10px] text-cortex-400">{$t('stats.aboveCer')}</div>
        </div>
      </div>
      {#if !quality.qualityGatePassed}
        <p class="text-[10px] text-red-400">{$t('stats.qualityGateFailed')}</p>
      {/if}
    {/if}
    {#if quality.duplicateGroups.length > 0}
      <div class="text-[10px] text-cortex-500 space-y-1 max-h-20 overflow-y-auto">
        {#each quality.duplicateGroups.slice(0, 3) as group}
          <div class="truncate" dir="rtl" lang="ckb">
            "<bdi>{group.normalizedPreview}</bdi>" —
            <bdi>{group.segmentIds.length} {$t('stats.segShort')}</bdi>
          </div>
        {/each}
      </div>
    {/if}
  </div>
{/if}
