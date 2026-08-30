<script lang="ts">
  import LoaderCircle from '@lucide/svelte/icons/loader-circle';
  import AudioPlayer from './AudioPlayer.svelte';
  import ErrorBoundary from './ErrorBoundary.svelte';
  import { t } from './i18n';
  import { settings } from './stores/settingsStore';
  import { selectedSegment, wordTimestamps } from './stores/segmentStore';
  import { isProcessing } from './stores/uiStore';
  import Waveform from './Waveform.svelte';

  let {
    waveformData,
    waveformError,
    currentTime = $bindable(),
    playerDuration = $bindable(),
    isAudioPlaying = $bindable(),
    chunkClipPosition,
    chunkClipLength,
    chunkStartTime,
    chunkEndTime,
    chunkLabel,
    wordStartOverride,
    wordEndOverride,
    showHotkeyOverlay,
    onSeek,
    onRetryWaveform,
    onTranscribe,
  }: {
    waveformData: number[];
    waveformError: string | null;
    currentTime: number;
    playerDuration: number;
    isAudioPlaying: boolean;
    chunkClipPosition: number;
    chunkClipLength: number;
    chunkStartTime: number;
    chunkEndTime: number;
    chunkLabel: string | null;
    wordStartOverride: number | null;
    wordEndOverride: number | null;
    showHotkeyOverlay: boolean;
    onSeek: (time: number) => void;
    onRetryWaveform: () => void;
    onTranscribe: () => void;
  } = $props();
</script>

{#if $selectedSegment}
  <div class="card overflow-hidden">
    {#if waveformError}
      <div
        class="flex items-center justify-between gap-3 p-3 text-xs text-amber-300"
        data-testid="curate-waveform-error"
        role="status"
      >
        <span class="min-w-0 truncate">{$t('review.waveformFailed')}</span>
        <button type="button" class="btn btn-secondary shrink-0 !text-xs" onclick={onRetryWaveform}>
          {$t('retry')}
        </button>
      </div>
    {:else}
      <Waveform
        waveform={waveformData}
        currentTime={chunkClipPosition}
        duration={chunkClipLength}
        wordTimestamps={$wordTimestamps}
        {onSeek}
      />
    {/if}
  </div>

  <ErrorBoundary>
    <AudioPlayer
      audioPath={$selectedSegment.audioPath}
      startTime={wordStartOverride ?? chunkStartTime}
      endTime={wordEndOverride ?? chunkEndTime}
      displayStart={chunkStartTime}
      displayEnd={chunkEndTime}
      bind:currentTime
      bind:duration={playerDuration}
      bind:playing={isAudioPlaying}
      autoplay={$settings.autoplaySegments}
    />
  </ErrorBoundary>

  <div class="card p-4 space-y-3">
    <div class="flex items-center justify-between">
      <h2 class="text-sm font-semibold text-cortex-200 uppercase tracking-wider">
        {$t('transcript')}
        {#if chunkLabel}
          <span
            class="ms-2 text-[10px] font-normal normal-case text-cortex-500 bg-cortex-900 px-1.5 py-0.5 rounded"
          >
            {$t('chunk')}
            {chunkLabel}
          </span>
        {/if}
      </h2>
      <div class="flex gap-2">
        <button
          data-testid="transcribe-btn"
          class="btn btn-secondary !text-xs relative"
          onclick={onTranscribe}
          disabled={$isProcessing}
        >
          {#if $isProcessing}
            <span class="flex items-center gap-1">
              <LoaderCircle class="h-3 w-3 animate-spin" aria-hidden="true" />
              {$t('transcribing')}
            </span>
          {:else}
            {$t('transcribe')}
          {/if}
          {#if showHotkeyOverlay}
            <span
              class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
              >^T</span
            >
          {/if}
        </button>
      </div>
    </div>

    <div class="grid grid-cols-2 gap-3">
      <div class="space-y-1">
        <label for="raw-ts" class="text-[11px] text-cortex-400">{$t('rawAsr')}</label>
        <textarea
          id="raw-ts"
          dir="rtl"
          lang="ckb"
          class="input h-28 resize-none font-mono text-xs text-end"
          value={$selectedSegment.rawTranscript}
          readonly
        ></textarea>
      </div>
      <div class="space-y-1">
        <label for="norm-ts" class="text-[11px] text-cortex-400">{$t('normalized')}</label>
        <textarea
          id="norm-ts"
          dir="rtl"
          lang="ckb"
          class="input h-28 resize-none font-mono text-xs text-end"
          value={$selectedSegment.normalizedTranscript ?? ''}
          readonly
        ></textarea>
      </div>
    </div>
  </div>
{/if}
