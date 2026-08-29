<script lang="ts">
  import { t } from './i18n';
  import { effectiveTranscript } from './segmentQuality';
  import type { SpeechSegment } from './types';

  let {
    loading,
    queue,
    targetError = $bindable(),
    confidenceLevel = $bindable(),
    queueLimit = $bindable(),
    onJump,
  }: {
    loading: boolean;
    queue: SpeechSegment[];
    targetError: number;
    confidenceLevel: number;
    queueLimit: number;
    onJump: (segmentId: string) => void;
  } = $props();
</script>

<div class="space-y-4">
  <div class="bg-cortex-900/30 p-3 rounded-lg border border-cortex-800/50 space-y-2">
    <h3 class="text-xs font-semibold text-cortex-200">
      {$t('validation.activeLearning.title')}
    </h3>
    <p class="text-xs text-cortex-400 leading-relaxed">
      {$t('validation.activeLearning.description')}
    </p>
    <div class="grid grid-cols-3 gap-3 pt-2">
      <div class="space-y-1">
        <label for="target-error" class="text-[10px] text-cortex-400"
          >{$t('validation.activeLearning.targetError')}</label
        >
        <input
          id="target-error"
          type="number"
          step="0.01"
          min="0.01"
          max="0.50"
          class="input !text-xs !py-1 px-2 font-mono"
          bind:value={targetError}
        />
      </div>
      <div class="space-y-1">
        <label for="confidence-level" class="text-[10px] text-cortex-400"
          >{$t('validation.activeLearning.confidence')}</label
        >
        <input
          id="confidence-level"
          type="number"
          step="0.01"
          min="0.50"
          max="0.99"
          class="input !text-xs !py-1 px-2 font-mono"
          bind:value={confidenceLevel}
        />
      </div>
      <div class="space-y-1">
        <label for="queue-limit" class="text-[10px] text-cortex-400"
          >{$t('validation.activeLearning.limit')}</label
        >
        <input
          id="queue-limit"
          type="number"
          step="1"
          min="5"
          max="100"
          class="input !text-xs !py-1 px-2 font-mono"
          bind:value={queueLimit}
        />
      </div>
    </div>
  </div>

  {#if loading}
    <div class="space-y-3">
      {#each [1, 2, 3] as _}<div
          class="h-12 bg-cortex-800/30 rounded-lg animate-pulse"
        ></div>{/each}
    </div>
  {:else if queue.length > 0}
    <ul class="space-y-2 p-0 list-none m-0">
      {#each queue as segment}
        <li
          class="rounded-lg border border-cortex-800/40 bg-cortex-900/20 p-3 text-xs text-cortex-200"
        >
          <div class="flex items-start justify-between gap-4">
            <div class="space-y-1 min-w-0">
              <div class="flex items-center gap-2">
                <span class="font-mono font-medium truncate text-cortex-100"
                  >{segment.audioPath.split(/[/\\]/).pop()}</span
                >
                {#if segment.verified}
                  <span class="badge-verified shrink-0">{$t('verified')}</span>
                {:else}
                  <span class="badge-pending shrink-0">{$t('pending')}</span>
                {/if}
              </div>
              <p class="text-cortex-350 italic truncate mt-1" dir="rtl" lang="ckb">
                "<bdi>{effectiveTranscript(segment)}</bdi>"
              </p>
              <div
                class="flex flex-wrap items-center gap-x-4 gap-y-1 text-[10px] text-cortex-400 pt-1"
              >
                <span>{$t('duration')}: {(segment.durationMs / 1000).toFixed(2)}s</span>
                {#if segment.confidence != null}
                  <span>Confidence: {segment.confidence.toFixed(2)}</span>
                {/if}
                {#if segment.ctcScore != null}<span>CTC Match: {segment.ctcScore.toFixed(2)}</span
                  >{/if}
                {#if segment.signalAnomalyScore != null}
                  <span
                    >{$t('validation.signalAnomaly.score')}: {segment.signalAnomalyScore.toFixed(
                      2,
                    )}</span
                  >
                {/if}
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
  {:else}
    <div class="text-center py-8 text-cortex-500 text-xs italic">
      {$t('validation.activeLearning.noSegments')}
    </div>
  {/if}
</div>
