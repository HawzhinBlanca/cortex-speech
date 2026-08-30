<script lang="ts">
  import CircleCheckBig from '@lucide/svelte/icons/circle-check-big';
  import Mic from '@lucide/svelte/icons/mic';
  import Search from '@lucide/svelte/icons/search';
  import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
  import { segmentChunkLabel, segmentSourceFilename, truncateFilename } from './alignment';
  import { t } from './i18n';
  import { effectiveTranscript } from './segmentQuality';
  import {
    filteredSegments,
    libraryLoadError,
    libraryTruncated,
    searchQuery,
    segments,
    selectedSegmentId,
  } from './stores/segmentStore';
  import { isProcessing } from './stores/uiStore';
  import type { SpeechSegment } from './types';
  import ErrorBoundary from './ErrorBoundary.svelte';
  import PanelSplitter from './PanelSplitter.svelte';
  import SearchBar from './SearchBar.svelte';
  import VirtualList from './VirtualList.svelte';

  let {
    sidebarOpen = $bindable(),
    sidebarWidth = $bindable(),
    batchSpeakerId = $bindable(),
    tauriAvailable,
    segmentsLoading,
    showHotkeyOverlay,
    onBatchTranscribe,
    onBatchAssignSpeaker,
    onBatchNormalize,
    onRediarize,
    onOpenSpeaker,
    onOpenDatasetMerge,
    onDeleteFiltered,
    onSelectSegment,
    onLoadSegments,
    onImport,
    onOpenFile,
  }: {
    sidebarOpen: boolean;
    sidebarWidth: number;
    batchSpeakerId: string;
    tauriAvailable: boolean;
    segmentsLoading: boolean;
    showHotkeyOverlay: boolean;
    onBatchTranscribe: (mode: 'empty' | 'selected' | 'filtered') => void;
    onBatchAssignSpeaker: () => void;
    onBatchNormalize: () => void;
    onRediarize: (mode: 'selected' | 'filtered') => void;
    onOpenSpeaker: () => void;
    onOpenDatasetMerge: () => void;
    onDeleteFiltered: () => void;
    onSelectSegment: (segment: SpeechSegment) => void;
    onLoadSegments: () => void;
    onImport: () => void;
    onOpenFile: () => void;
  } = $props();

  function formatDuration(milliseconds: number): string {
    const minutes = Math.floor(milliseconds / 60000);
    const seconds = Math.floor((milliseconds % 60000) / 1000);
    return `${minutes}:${seconds.toString().padStart(2, '0')}`;
  }
</script>

<ErrorBoundary>
  <aside
    data-testid="left-panel"
    class="shrink-0 flex flex-col border-r border-cortex-800/30 bg-cortex-900/40 backdrop-blur-md transition-all duration-200 overflow-hidden"
    class:panel-collapsed={!sidebarOpen}
    style="width: {sidebarWidth}px;"
  >
    {#if sidebarOpen}
      <div class="p-2 space-y-2 relative">
        <SearchBar />
        {#if showHotkeyOverlay}
          <span
            class="absolute top-4 right-4 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
            >^F</span
          >
        {/if}
        <div class="flex flex-wrap gap-1">
          <button
            class="btn btn-secondary btn-sm !text-[10px] flex-1"
            onclick={() => onBatchTranscribe('empty')}
            disabled={!tauriAvailable ||
              $isProcessing ||
              $segments.every((segment) => segment.rawTranscript?.trim())}
            title={tauriAvailable ? $t('batchTranscribe.empty') : $t('desktopRuntimeRequired')}
            >{$t('batchTranscribe.empty')}</button
          >
          <button
            class="btn btn-secondary btn-sm !text-[10px] flex-1"
            onclick={() => onBatchTranscribe('selected')}
            disabled={!tauriAvailable || $isProcessing || !$selectedSegmentId}
            title={tauriAvailable ? $t('batchTranscribe.selected') : $t('desktopRuntimeRequired')}
            >{$t('batchTranscribe.selected')}</button
          >
          <button
            class="btn btn-secondary btn-sm !text-[10px] flex-1"
            onclick={() => onBatchTranscribe('filtered')}
            disabled={!tauriAvailable || $isProcessing || $filteredSegments.length === 0}
            title={tauriAvailable ? $t('batchTranscribe.filtered') : $t('desktopRuntimeRequired')}
            >{$t('batchTranscribe.filtered')}</button
          >
        </div>
        <div class="flex flex-wrap gap-1 items-center">
          <input
            class="input !text-[10px] flex-1 !py-1 !px-2 font-mono"
            placeholder={$t('batchAssignSpeaker.placeholder')}
            bind:value={batchSpeakerId}
            aria-label={$t('batchAssignSpeaker.placeholder')}
          />
          <button
            class="btn btn-secondary btn-sm !text-[10px] shrink-0"
            onclick={onBatchAssignSpeaker}
            disabled={!tauriAvailable || $isProcessing || $filteredSegments.length === 0}
            title={tauriAvailable ? $t('batchAssignSpeaker.label') : $t('desktopRuntimeRequired')}
            >{$t('batchAssignSpeaker.label')}</button
          >
        </div>
        <div class="flex flex-wrap gap-1">
          <button
            class="btn btn-secondary btn-sm !text-[10px] flex-1"
            onclick={onBatchNormalize}
            disabled={!tauriAvailable ||
              $isProcessing ||
              !$filteredSegments.some((segment) => segment.rawTranscript?.trim())}
            title={tauriAvailable ? $t('batchNormalize.label') : $t('desktopRuntimeRequired')}
            >{$t('batchNormalize.label')}</button
          >
          <button
            class="btn btn-secondary btn-sm !text-[10px] flex-1"
            onclick={() => onRediarize('filtered')}
            disabled={!tauriAvailable || $isProcessing || $filteredSegments.length === 0}
            title={tauriAvailable ? $t('rediarize.filtered') : $t('desktopRuntimeRequired')}
            >{$t('rediarize.filtered')}</button
          >
          <button
            class="btn btn-secondary btn-sm !text-[10px] flex-1"
            onclick={() => onRediarize('selected')}
            disabled={!tauriAvailable || $isProcessing || !$selectedSegmentId}
            title={tauriAvailable ? $t('rediarize.selected') : $t('desktopRuntimeRequired')}
            >{$t('rediarize.selected')}</button
          >
        </div>
        <div class="flex flex-wrap gap-1 border-t border-cortex-800/30 pt-2">
          <button
            class="btn btn-secondary btn-sm !text-[10px] flex-1"
            onclick={onOpenSpeaker}
            disabled={!tauriAvailable || $isProcessing}
            title={tauriAvailable ? $t('speaker.title') : $t('desktopRuntimeRequired')}
            >{$t('speakers')}</button
          >
          <button
            class="btn btn-secondary btn-sm !text-[10px] flex-1"
            onclick={onOpenDatasetMerge}
            disabled={!tauriAvailable || $isProcessing}
            title={tauriAvailable ? $t('merge.title') : $t('desktopRuntimeRequired')}
            >{$t('merge')}</button
          >
          <button
            class="btn btn-danger btn-sm !text-[10px] flex-1"
            onclick={onDeleteFiltered}
            disabled={!tauriAvailable || $isProcessing || $filteredSegments.length === 0}
            title={tauriAvailable ? $t('batchDelete.filtered') : $t('desktopRuntimeRequired')}
            >{$t('batchDelete.filtered')}</button
          >
        </div>
      </div>
      <div class="flex-1 overflow-hidden p-2 pt-0">
        <VirtualList
          items={$filteredSegments}
          itemHeight={56}
          selectedId={$selectedSegmentId}
          onSelect={onSelectSegment}
          hasMore={$libraryTruncated}
          onEndReached={() => void segments.loadMore()}
        >
          {#snippet children(item: SpeechSegment)}
            {@const sourceName = truncateFilename(segmentSourceFilename(item.audioPath))}
            {@const chunkBadge = segmentChunkLabel(item.alignmentJson)}
            <button
              data-testid="segment-card"
              data-id={item.id}
              class="w-full text-start p-2.5 rounded-xl transition-all duration-300 h-full flex items-start group
                {item.id === $selectedSegmentId
                ? 'bg-gradient-to-br from-cortex-800/80 to-cortex-900/80 ring-1 ring-cortex-400 shadow-[0_0_15px_rgba(56,189,248,0.15)] scale-[1.02] transform'
                : 'hover:bg-cortex-800/40 hover:scale-[1.01] transform'}"
              onclick={() => onSelectSegment(item)}
            >
              <div class="flex-1 min-w-0">
                <div class="flex items-center gap-2 min-w-0">
                  <span
                    class="text-xs font-semibold truncate flex-1 min-w-0 transition-colors {item.id ===
                    $selectedSegmentId
                      ? 'text-cortex-100'
                      : 'text-cortex-200 group-hover:text-cortex-300'}"
                    title={item.audioPath}>{sourceName}</span
                  >
                  {#if chunkBadge}
                    <span
                      class="text-[9px] text-cortex-400 bg-cortex-900/80 border border-cortex-800/50 px-1.5 py-0.5 rounded shadow-sm shrink-0 font-mono"
                      title="{$t('chunk')} {chunkBadge}">{chunkBadge}</span
                    >
                  {/if}
                  {#if item.verified}
                    <CircleCheckBig
                      class="h-3 w-3 shrink-0 text-emerald-400 drop-shadow-[0_0_5px_rgba(52,211,153,0.5)]"
                      role="img"
                      aria-label={$t('verified')}
                    />
                  {/if}
                </div>
                <div class="flex items-center gap-2 mt-1">
                  <span
                    class="text-[10px] text-cortex-400 font-medium bg-cortex-950/50 px-1.5 rounded-sm shrink-0"
                  >
                    {formatDuration(item.durationMs)}
                  </span>
                  {#if item.confidence !== undefined && item.confidence !== null}
                    <span
                      class="text-[10px] font-mono font-medium px-1.5 rounded-sm border shrink-0
                        {item.confidence < 0.5
                        ? 'text-red-400 bg-red-950/30 border-red-900/30'
                        : item.confidence < 0.85
                          ? 'text-amber-400 bg-amber-950/30 border-amber-900/30'
                          : 'text-emerald-400 bg-emerald-950/30 border-emerald-900/30'}"
                      title={$t('validation.activeLearning.confidence')}
                      >{Math.round(item.confidence * 100)}%</span
                    >
                  {/if}
                  {#if item.speakerId}
                    <span
                      class="text-[10px] text-indigo-300 font-medium bg-indigo-950/30 border border-indigo-900/50 px-1.5 rounded-sm truncate max-w-24 shrink-0"
                    >
                      {item.speakerId}
                    </span>
                  {/if}
                  <span class="text-[11px] text-cortex-500 truncate mt-0.5" dir="rtl" lang="ckb">
                    {effectiveTranscript(item) || '...'}
                  </span>
                </div>
              </div>
            </button>
          {/snippet}
        </VirtualList>

        {#if segmentsLoading}
          <div class="space-y-2 p-2">
            {#each [1, 2, 3, 4, 5] as _}
              <div
                class="p-2 rounded-xl bg-cortex-950/20 border border-cortex-900/10 space-y-1.5 animate-pulse"
              >
                <div class="flex items-center justify-between">
                  <div class="h-3 bg-cortex-800/30 rounded-md w-2/3"></div>
                  <div class="h-3 bg-cortex-800/30 rounded-md w-8"></div>
                </div>
                <div class="flex gap-2">
                  <div class="h-2.5 bg-cortex-800/15 rounded-md w-10"></div>
                  <div class="h-2.5 bg-cortex-800/15 rounded-md w-1/2"></div>
                </div>
              </div>
            {/each}
          </div>
        {:else if $filteredSegments.length === 0}
          <div
            data-testid="segments-empty-state"
            class="flex min-h-full flex-col items-center [justify-content:safe_center] gap-3 px-6 text-center animate-fade-in"
          >
            {#if $libraryLoadError}
              <div
                data-testid="segments-load-error"
                class="flex h-14 w-14 items-center justify-center rounded-2xl bg-surface-2 text-danger"
              >
                <TriangleAlert size={26} strokeWidth={1.5} aria-hidden="true" />
              </div>
              <div class="max-w-[16rem]">
                <p class="text-sm font-semibold text-default">
                  {$t('notifications.loadSegmentsFailed')}
                </p>
                <p class="mt-1 break-words text-xs leading-relaxed text-muted">
                  {$libraryLoadError}
                </p>
              </div>
              <div class="mt-1">
                <button class="btn btn-primary !text-xs" onclick={onLoadSegments}
                  >{$t('retry')}</button
                >
              </div>
            {:else if $searchQuery}
              <div
                class="flex h-12 w-12 items-center justify-center rounded-full bg-surface-2 text-subtle"
              >
                <Search size={22} strokeWidth={1.5} aria-hidden="true" />
              </div>
              <div>
                <p class="text-sm font-medium text-default">{$t('noResultsFound')}</p>
                <p class="mt-1 max-w-[14rem] truncate text-xs text-subtle">“{$searchQuery}”</p>
              </div>
            {:else}
              <div
                class="flex h-14 w-14 items-center justify-center rounded-2xl bg-accent-soft text-accent"
              >
                <Mic size={26} strokeWidth={1.5} aria-hidden="true" />
              </div>
              <div class="max-w-[15rem]">
                <p class="text-sm font-semibold text-default">{$t('noSegmentsLoaded')}</p>
                <p class="mt-1 text-xs leading-relaxed text-muted">{$t('emptyStateHint')}</p>
              </div>
              {#if tauriAvailable}
                <div class="mt-1 flex gap-2">
                  <button class="btn btn-primary !text-xs" onclick={onImport}>{$t('import')}</button
                  >
                  <button class="btn btn-secondary !text-xs" onclick={onOpenFile}
                    >{$t('open')}</button
                  >
                </div>
              {/if}
            {/if}
          </div>
        {/if}
      </div>
    {/if}
  </aside>
</ErrorBoundary>
<PanelSplitter
  direction="horizontal"
  label={$t('resizeSegmentsPanel')}
  value={sidebarWidth}
  onResize={(delta) => (sidebarWidth = Math.max(200, Math.min(600, sidebarWidth + delta)))}
/>
