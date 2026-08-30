<script lang="ts">
  import { tick } from 'svelte';
  import { t } from './i18n';
  import type { WordTimestamp } from './types';

  interface Props {
    words: WordTimestamp[];
    activeWordIndex: number;
    editingWordIndex?: number | null;
    chipText: (word: WordTimestamp, index: number) => string;
    confidenceClass: (confidence: number | null | undefined) => string;
    isEdited: (index: number) => boolean;
    editingDisabled?: boolean;
    editingDisabledDescriptionId?: string;
    onReplay: () => void;
    onPlay: (word: WordTimestamp) => void;
    onStartEdit: (word: WordTimestamp, index: number) => void;
    onCommitEdit: (index: number, word: WordTimestamp, value: string, viaBlur?: boolean) => boolean;
    onCancelEdit: (index: number) => void;
  }

  let {
    words,
    activeWordIndex,
    editingWordIndex = $bindable(null),
    chipText,
    confidenceClass,
    isEdited,
    editingDisabled = false,
    editingDisabledDescriptionId,
    onReplay,
    onPlay,
    onStartEdit,
    onCommitEdit,
    onCancelEdit,
  }: Props = $props();

  let stripElement: HTMLElement | undefined;

  function focusOnMount(node: HTMLInputElement) {
    node.focus();
    node.select();
  }

  function refocusChip(index: number) {
    void tick().then(() =>
      stripElement?.querySelector<HTMLButtonElement>(`[data-chip="${index}"]`)?.focus(),
    );
  }
</script>

<div class="review-secondary card p-4">
  <div class="flex items-center justify-between gap-3">
    <div>
      <div class="text-xs font-semibold uppercase tracking-wider text-muted">
        {$t('review.listen')}
      </div>
      <p class="mt-0.5 text-xs text-subtle">{$t('review.listenHint')}</p>
    </div>
    <button
      type="button"
      class="ring-focus shrink-0 rounded-token px-2 py-1 text-xs text-subtle transition-colors hover:text-default"
      onclick={onReplay}
      disabled={editingDisabled}
      aria-describedby={editingDisabled ? editingDisabledDescriptionId : undefined}
    >
      {$t('review.replay')}
    </button>
  </div>
  <div
    bind:this={stripElement}
    dir="rtl"
    class="font-kurdish mt-3 flex flex-wrap items-center gap-x-1 gap-y-2 text-2xl leading-loose"
  >
    {#each words as word, index (index)}
      {#if editingWordIndex === index}
        <input
          type="text"
          dir="rtl"
          lang="ckb"
          class="review-word-input"
          style={`width: ${Math.max(5, chipText(word, index).length + 3)}ch`}
          value={chipText(word, index)}
          aria-label={$t('review.editWordAria')}
          disabled={editingDisabled}
          aria-describedby={editingDisabled ? editingDisabledDescriptionId : undefined}
          onblur={(event) =>
            onCommitEdit(index, word, (event.target as HTMLInputElement).value, true)}
          onkeydown={(event) => {
            event.stopPropagation();
            if (event.key === 'Enter') {
              event.preventDefault();
              if (onCommitEdit(index, word, (event.target as HTMLInputElement).value)) {
                refocusChip(index);
              }
            } else if (event.key === 'Escape') {
              onCancelEdit(index);
              refocusChip(index);
            }
          }}
          use:focusOnMount
        />
      {:else}
        <button
          type="button"
          disabled={editingDisabled}
          aria-describedby={editingDisabled ? editingDisabledDescriptionId : undefined}
          data-chip={index}
          class="review-word {isEdited(index)
            ? 'word-edited'
            : confidenceClass(word.confidence)} {index === activeWordIndex ? 'word-active' : ''}"
          onclick={() => onPlay(word)}
          ondblclick={() => onStartEdit(word, index)}
          onkeydown={(event) => {
            if (event.key === 'Enter' || event.key === ' ') {
              event.preventDefault();
              onPlay(word);
            } else if (event.key === 'F2') {
              event.preventDefault();
              onStartEdit(word, index);
            }
          }}
          title={`${word.start.toFixed(2)}s · ${Math.round((word.confidence ?? 1) * 100)}% — ${$t('review.wordChipHint')}`}
        >
          {chipText(word, index)}
        </button>
      {/if}
    {/each}
  </div>
</div>

<style>
  .review-word {
    border-radius: 0.375rem;
    padding: 0.05rem 0.4rem;
    color: var(--text);
    cursor: pointer;
    transition:
      background-color 120ms ease,
      color 120ms ease;
  }
  .review-word:hover {
    background: var(--surface-3);
  }
  .review-word:disabled {
    cursor: not-allowed;
    opacity: 0.6;
  }
  .conf-mid {
    background: color-mix(in srgb, var(--warning) 18%, transparent);
  }
  .conf-low {
    background: color-mix(in srgb, var(--danger) 20%, transparent);
  }
  .review-word.word-active {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    box-shadow: inset 0 0 0 2px var(--accent);
    font-weight: 700;
  }
  .review-word.word-edited {
    background: color-mix(in srgb, var(--success, #22c55e) 16%, transparent);
  }
  .review-word-input {
    font: inherit;
    border-radius: 0.375rem;
    padding: 0.05rem 0.4rem;
    color: var(--text);
    background: var(--surface-3);
    border: none;
    outline: none;
    box-shadow: inset 0 0 0 2px var(--accent);
    text-align: right;
  }
</style>
