<script lang="ts">
  import SquarePen from '@lucide/svelte/icons/square-pen';
  import X from '@lucide/svelte/icons/x';
  import type { SegmentMetadataFields } from './commands';
  import EmptyState from './EmptyState.svelte';
  import ErrorBoundary from './ErrorBoundary.svelte';
  import { t } from './i18n';
  import LazyComponent from './LazyComponent.svelte';
  import { segmentStats, selectedSegment } from './stores/segmentStore';
  import type { WordTimestamp } from './types';
  import WorkstationAnnotationPanel from './WorkstationAnnotationPanel.svelte';
  import WorkstationTranscriptPanel from './WorkstationTranscriptPanel.svelte';

  type ViewMode = 'curate' | 'insights' | 'review';
  type LazyLabels = {
    loadingLabel: string;
    failedLabel: string;
    retryLabel: string;
    closeLabel: string;
  };

  let {
    viewMode,
    reviewNudgeDismissed = $bindable(),
    editorTab = $bindable(),
    currentTime = $bindable(),
    playerDuration = $bindable(),
    isAudioPlaying = $bindable(),
    waveformData,
    waveformError,
    chunkClipPosition,
    chunkClipLength,
    chunkStartTime,
    chunkEndTime,
    chunkLabel,
    wordStartOverride,
    wordEndOverride,
    selectedMetadataReady,
    showHotkeyOverlay,
    modKey,
    lazyLabels,
    loadStatsDashboard,
    loadRefineryPanel,
    loadReviewMode,
    onEnterReview,
    onLeaveReview,
    onExport,
    onRetryWaveform,
    onSeek,
    onTranscribe,
    onPlayWord,
    onScheduleAutoSave,
    onSaveSpeaker,
    onAlign,
    onDelete,
    onOpenReviewInbox,
  }: {
    viewMode: ViewMode;
    reviewNudgeDismissed: boolean;
    editorTab: 'interactive' | 'raw';
    currentTime: number;
    playerDuration: number;
    isAudioPlaying: boolean;
    waveformData: number[];
    waveformError: string | null;
    chunkClipPosition: number;
    chunkClipLength: number;
    chunkStartTime: number;
    chunkEndTime: number;
    chunkLabel: string | null;
    wordStartOverride: number | null;
    wordEndOverride: number | null;
    selectedMetadataReady: boolean;
    showHotkeyOverlay: boolean;
    modKey: string;
    lazyLabels: LazyLabels;
    loadStatsDashboard: () => Promise<unknown>;
    loadRefineryPanel: () => Promise<unknown>;
    loadReviewMode: () => Promise<unknown>;
    onEnterReview: () => void;
    onLeaveReview: () => Promise<void>;
    onExport: () => void;
    onRetryWaveform: () => void;
    onSeek: (time: number) => void;
    onTranscribe: () => void;
    onPlayWord: (word: WordTimestamp) => void;
    onScheduleAutoSave: (edits: SegmentMetadataFields) => void;
    onSaveSpeaker: () => void;
    onAlign: () => void;
    onDelete: () => void;
    onOpenReviewInbox: () => void;
  } = $props();
</script>

<ErrorBoundary>
  <main data-testid="center-panel" class="flex-1 flex flex-col gap-3 p-4 overflow-y-auto min-w-0">
    {#if viewMode === 'curate' && $segmentStats.pending > 0 && !reviewNudgeDismissed}
      <div
        data-testid="review-nudge"
        class="shrink-0 flex items-center justify-between gap-3 rounded-lg border border-amber-400/40 bg-amber-400/10 px-4 py-3"
      >
        <div class="flex items-center gap-2.5 text-sm text-amber-100">
          <SquarePen class="h-5 w-5 shrink-0" aria-hidden="true" />
          <span>
            {$t($segmentStats.pending === 1 ? 'reviewCorrect.ctaOne' : 'reviewCorrect.cta', {
              n: String($segmentStats.pending),
            })}
          </span>
        </div>
        <div class="flex items-center gap-2 shrink-0">
          <button
            data-testid="review-nudge-start"
            class="btn btn-primary !text-xs"
            onclick={onEnterReview}
          >
            {$t('reviewCorrect.start')}
          </button>
          <button
            class="text-cortex-400 hover:text-cortex-200 text-sm leading-none px-1"
            aria-label={$t('reviewCorrect.dismiss')}
            title={$t('reviewCorrect.dismiss')}
            onclick={() => (reviewNudgeDismissed = true)}
          >
            <X class="h-4 w-4" aria-hidden="true" />
          </button>
        </div>
      </div>
    {/if}

    {#if viewMode === 'insights'}
      <LazyComponent
        load={loadStatsDashboard}
        componentProps={{ onOpenReview: onEnterReview }}
        {...lazyLabels}
      />
      <LazyComponent load={loadRefineryPanel} {...lazyLabels} />
    {:else if viewMode === 'review'}
      <LazyComponent
        load={loadReviewMode}
        componentProps={{ onExport, onDone: () => void onLeaveReview() }}
        {...lazyLabels}
        onClose={onLeaveReview}
      />
    {:else if $selectedSegment}
      <WorkstationTranscriptPanel
        {waveformData}
        {waveformError}
        bind:currentTime
        bind:playerDuration
        bind:isAudioPlaying
        {chunkClipPosition}
        {chunkClipLength}
        {chunkStartTime}
        {chunkEndTime}
        {chunkLabel}
        {wordStartOverride}
        {wordEndOverride}
        {showHotkeyOverlay}
        {onSeek}
        {onRetryWaveform}
        {onTranscribe}
      />
      <WorkstationAnnotationPanel
        bind:editorTab
        {currentTime}
        {chunkStartTime}
        {selectedMetadataReady}
        {showHotkeyOverlay}
        {onPlayWord}
        {onScheduleAutoSave}
        {onSaveSpeaker}
        {onAlign}
        {onDelete}
      />
    {:else}
      <EmptyState variant="mic" title={$t('selectSegment')}>
        {#if $segmentStats.pending > 0}
          <div class="flex flex-col items-center gap-2 mb-4">
            <p class="text-sm text-default">
              {$segmentStats.pending === 1
                ? $t('reviewCorrect.ctaOne')
                : $t('reviewCorrect.cta', { n: String($segmentStats.pending) })}
            </p>
            <button
              class="btn btn-primary"
              onclick={onOpenReviewInbox}
              data-testid="empty-start-review"
            >
              {$t('reviewCorrect.start')}
            </button>
          </div>
        {/if}
        <div class="flex flex-wrap justify-center gap-x-3 gap-y-1 text-xs text-subtle">
          <span><kbd>{modKey}+O</kbd> {$t('openFile')}</span>
          <span><kbd>{modKey}+I</kbd> {$t('import')}</span>
          <span><kbd>{modKey}+T</kbd> {$t('transcribe')}</span>
          <span><kbd>{modKey}+K</kbd> {$t('shortcuts')}</span>
        </div>
      </EmptyState>
    {/if}
  </main>
</ErrorBoundary>
