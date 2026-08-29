<script lang="ts">
  import { t } from './i18n';
  import { effectiveTranscript } from './segmentQuality';
  import type { SpeechSegment } from './types';

  let {
    running,
    segments,
    showAll = $bindable(false),
    onJump,
  }: {
    running: boolean;
    segments: SpeechSegment[];
    showAll?: boolean;
    onJump: (segmentId: string) => void;
  } = $props();

  const displayedSegments = $derived(
    showAll ? segments : segments.filter((segment) => (segment.signalAnomalyScore || 0) > 0.35),
  );
</script>

<div class="space-y-4">
  <div class="bg-cortex-900/30 p-3 rounded-lg border border-cortex-800/50 space-y-2">
    <h3 class="text-xs font-semibold text-cortex-200">{$t('validation.signalAnomaly.title')}</h3>
    <p class="text-xs text-cortex-400 leading-relaxed">
      {$t('validation.signalAnomaly.description')}
    </p>
    <div class="flex items-center justify-between pt-1">
      <label class="flex items-center gap-2 text-[11px] text-cortex-300 cursor-pointer select-none">
        <input
          type="checkbox"
          class="rounded border-cortex-800 bg-cortex-950 text-cortex-500 focus:ring-0"
          bind:checked={showAll}
        />
        {$t('validation.signalAnomaly.showAll')}
      </label>
    </div>
  </div>

  {#if running}
    <div class="space-y-3">
      {#each [1, 2, 3] as _}<div
          class="h-12 bg-cortex-800/30 rounded-lg animate-pulse"
        ></div>{/each}
    </div>
  {:else if displayedSegments.length > 0}
    <ul class="space-y-2 p-0 list-none m-0">
      {#each displayedSegments as segment}
        {@const isFlagged = (segment.signalAnomalyScore || 0) > 0.35}
        <li
          class="rounded-lg border p-3 text-xs {isFlagged
            ? 'border-red-500/40 bg-red-950/20 text-red-200'
            : 'border-cortex-800/40 bg-cortex-900/20 text-cortex-200'}"
        >
          <div class="flex items-start justify-between gap-4">
            <div class="space-y-1 min-w-0">
              <div class="flex items-center gap-2">
                <span
                  class="font-mono font-medium truncate {isFlagged
                    ? 'text-red-100'
                    : 'text-cortex-100'}">{segment.audioPath.split(/[/\\]/).pop()}</span
                >
                {#if isFlagged}
                  <span
                    class="px-1.5 py-0.5 rounded bg-red-500/20 text-red-300 text-[9px] font-bold border border-red-500/30 uppercase shrink-0"
                    >{$t('validation.signalAnomaly.isSignalAnomaly')}</span
                  >
                {/if}
              </div>
              <p class="opacity-80 italic truncate mt-1" dir="rtl" lang="ckb">
                "<bdi>{effectiveTranscript(segment)}</bdi>"
              </p>
              <div class="flex items-center gap-4 text-[10px] opacity-70 pt-1">
                <span class="font-semibold text-amber-400"
                  >{$t('validation.signalAnomaly.score')}: {segment.signalAnomalyScore?.toFixed(
                    3,
                  )}</span
                >
                <span>{$t('duration')}: {(segment.durationMs / 1000).toFixed(2)}s</span>
              </div>
            </div>
            <button
              class="btn-secondary !text-[10px] !px-2 !py-1 shrink-0"
              onclick={() => onJump(segment.id)}>{$t('validation.goToSegment')}</button
            >
          </div>
        </li>
      {/each}
    </ul>
  {:else if segments.length === 0}
    <div class="text-center py-8 text-cortex-500 text-xs italic">
      {$t('validation.signalAnomaly.notScreened')}
    </div>
  {:else}
    <div class="text-center py-8 text-cortex-500 text-xs italic">
      {$t('validation.signalAnomaly.noSignalAnomaly')}
    </div>
  {/if}
</div>
