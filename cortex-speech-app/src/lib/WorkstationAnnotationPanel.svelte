<script lang="ts">
  import DiffView from './DiffView.svelte';
  import { t } from './i18n';
  import { segments, selectedSegment, wordTimestamps } from './stores/segmentStore';
  import { isProcessing } from './stores/uiStore';
  import type { SegmentMetadataFields } from './commands';
  import type { WordTimestamp } from './types';

  let {
    editorTab = $bindable(),
    currentTime,
    chunkStartTime,
    selectedMetadataReady,
    showHotkeyOverlay,
    onPlayWord,
    onScheduleAutoSave,
    onSaveSpeaker,
    onAlign,
    onDelete,
  }: {
    editorTab: 'interactive' | 'raw';
    currentTime: number;
    chunkStartTime: number;
    selectedMetadataReady: boolean;
    showHotkeyOverlay: boolean;
    onPlayWord: (word: WordTimestamp) => void;
    onScheduleAutoSave: (edits: SegmentMetadataFields) => void;
    onSaveSpeaker: () => void;
    onAlign: () => void;
    onDelete: () => void;
  } = $props();
</script>

{#if $selectedSegment}
  <div class="card p-4 space-y-3">
    <div class="flex items-center justify-between">
      <div class="flex items-center gap-2">
        <h2 class="text-sm font-semibold text-cortex-200 uppercase tracking-wider">
          {$t('annotation')}
        </h2>
        {#if $selectedSegment.verified}
          <span class="badge-verified">{$t('verified')}</span>
        {:else}
          <span class="badge-pending">{$t('pending')}</span>
        {/if}
      </div>
      <div class="flex bg-cortex-950 p-0.5 rounded-lg border border-cortex-800/40">
        <button
          class="px-2.5 py-1 text-[10px] uppercase font-bold tracking-wider rounded-md transition-colors
            {editorTab === 'interactive'
            ? 'bg-cortex-700 text-default shadow-sm'
            : 'text-cortex-400 hover:text-cortex-200'}"
          onclick={() => (editorTab = 'interactive')}>{$t('editorInteractive')}</button
        >
        <button
          class="px-2.5 py-1 text-[10px] uppercase font-bold tracking-wider rounded-md transition-colors
            {editorTab === 'raw'
            ? 'bg-cortex-700 text-default shadow-sm'
            : 'text-cortex-400 hover:text-cortex-200'}"
          onclick={() => (editorTab = 'raw')}>{$t('annotation')}</button
        >
      </div>
    </div>

    {#if editorTab === 'interactive'}
      <div
        class="p-5 rounded-2xl bg-gradient-to-b from-cortex-900/50 to-cortex-950/80 border border-white/5 shadow-inner font-mono text-[15px] leading-loose min-h-32 select-text transition-all duration-300 hover:border-cortex-500/30 hover:shadow-[inset_0_0_20px_rgba(56,189,248,0.05)]"
      >
        {#if $wordTimestamps.length > 0}
          <div class="flex flex-wrap gap-x-1.5 gap-y-2" dir="rtl" lang="ckb">
            {#each $wordTimestamps as word}
              {@const isActive =
                currentTime - chunkStartTime >= word.start &&
                currentTime - chunkStartTime <= word.end}
              <span
                class="relative inline-block px-1.5 py-0.5 rounded cursor-pointer transition-all duration-150 group
                  {isActive
                  ? 'bg-cortex-700 text-default font-bold border-b border-yellow-400'
                  : 'text-cortex-200 hover:bg-cortex-800 hover:text-white'}"
                onclick={() => onPlayWord(word)}
                title="{word.word} ({word.start.toFixed(2)}s - {word.end.toFixed(2)}s)"
                role="button"
                tabindex="0"
                aria-keyshortcuts="Enter Space"
                onkeydown={(event) => {
                  if (event.key === 'Enter' || event.key === ' ') {
                    onPlayWord(word);
                    event.preventDefault();
                  }
                }}
              >
                <span class="select-text">{word.word}</span>
                <span
                  class="absolute -top-6 left-1/2 -translate-x-1/2 px-1.5 py-0.5 text-[8px] bg-cortex-950 text-cortex-400 rounded opacity-0 group-hover:opacity-100 transition-opacity pointer-events-none whitespace-nowrap z-10 border border-cortex-850 shadow-md"
                >
                  {word.start.toFixed(2)}s
                </span>
              </span>
            {/each}
          </div>
        {:else}
          <p class="text-cortex-500 italic">{$t('editor.noWordTimestamps')}</p>
        {/if}
      </div>
    {:else}
      <textarea
        dir="rtl"
        lang="ckb"
        class="input h-32 resize-none font-mono text-sm text-end"
        value={$selectedSegment.annotatedTranscript ?? ''}
        readonly
      ></textarea>
    {/if}

    <div class="flex items-end gap-2">
      <div class="flex-1 space-y-1">
        <label for="speaker-id" class="text-[11px] text-cortex-400">{$t('speaker')}</label>
        <input
          id="speaker-id"
          class="input !text-xs font-mono"
          value={$selectedSegment.speakerId ?? ''}
          placeholder={$t('batchAssignSpeaker.placeholder')}
          disabled={$isProcessing || !selectedMetadataReady}
          aria-describedby={!selectedMetadataReady ? 'speaker-metadata-loading' : undefined}
          oninput={(event) => {
            const segment = $selectedSegment;
            if (!segment) return;
            const speakerId = (event.target as HTMLInputElement).value;
            segments.update((rows) =>
              rows.map((row) => (row.id === segment.id ? { ...row, speakerId } : row)),
            );
            onScheduleAutoSave({ speakerId });
          }}
        />
      </div>
      <button
        class="btn btn-secondary !text-xs shrink-0"
        onclick={onSaveSpeaker}
        disabled={$isProcessing || !selectedMetadataReady}>{$t('speaker.save')}</button
      >
      {#if !selectedMetadataReady}
        <span id="speaker-metadata-loading" class="sr-only">{$t('loading')}</span>
      {/if}
    </div>

    <DiffView
      raw={$selectedSegment.rawTranscript ?? ''}
      annotated={$selectedSegment.annotatedTranscript ?? ''}
    />

    {#if $wordTimestamps.length > 0}
      <div class="space-y-1">
        <span class="text-[11px] text-cortex-400">{$t('wordTimestamps')}</span>
        <div
          class="flex flex-wrap gap-1 max-h-20 overflow-y-auto"
          role="group"
          aria-label={$t('wordTimestamps')}
          dir="rtl"
          lang="ckb"
        >
          {#each $wordTimestamps as word}
            <button
              type="button"
              class="px-1.5 py-0.5 text-[10px] rounded bg-cortex-800 text-cortex-300 font-mono cursor-pointer hover:bg-cortex-700 transition-colors border-0"
              title="{word.word}: {word.start.toFixed(2)}s - {word.end.toFixed(2)}s"
              onclick={() => onPlayWord(word)}
              aria-label={$t('review.playWordAria').replace('{word}', word.word)}
              >{word.word}</button
            >
          {/each}
        </div>
      </div>
    {/if}

    <div class="flex gap-2 pt-1">
      <button class="btn btn-secondary !text-xs" onclick={onAlign} disabled={$isProcessing}>
        {$t('align')}
      </button>
      <button class="btn btn-danger !text-xs ms-auto relative" onclick={onDelete}>
        {$t('delete')}
        {#if showHotkeyOverlay}
          <span
            class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
          >
            {$t('app.deleteHint')}
          </span>
        {/if}
      </button>
    </div>
  </div>
{/if}
