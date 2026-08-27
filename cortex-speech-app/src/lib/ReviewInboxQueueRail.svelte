<script lang="ts">
  import { t } from './i18n';
  import type { SpeechSegment } from './types';

  interface Props {
    queue: SpeechSegment[];
    currentIndex: number;
    current: SpeechSegment | null;
    queueTotal: number;
    nextCursor: string | null;
    isLoadingMore: boolean;
    loadMoreError: string | null;
    evictedCount: number;
    activeQueueAnnouncement: string;
    queueListbox?: HTMLUListElement | null;
    bandColor: (segment: SpeechSegment) => string;
    optionId: (index: number) => string;
    optionLabel: (segment: SpeechSegment, index: number) => string;
    onListboxKey: (event: KeyboardEvent) => void;
    onOptionKey: (event: KeyboardEvent, index: number) => void;
    onSelect: (index: number) => void;
    onLoadMore: () => void;
    onReloadStart: () => void;
  }

  let {
    queue,
    currentIndex,
    current,
    queueTotal,
    nextCursor,
    isLoadingMore,
    loadMoreError,
    evictedCount,
    activeQueueAnnouncement,
    queueListbox = $bindable(null),
    bandColor,
    optionId,
    optionLabel,
    onListboxKey,
    onOptionKey,
    onSelect,
    onLoadMore,
    onReloadStart,
  }: Props = $props();
</script>

<nav class="queue-rail" aria-label={$t('inbox.segmentQueue')}>
  <div class="rail-header" id="review-inbox-queue-label">
    {$t('inbox.queue', { n: String(queue.length) })}
  </div>
  <ul
    class="rail-list"
    role="listbox"
    tabindex="0"
    aria-labelledby="review-inbox-queue-label"
    aria-activedescendant={current ? optionId(currentIndex) : undefined}
    bind:this={queueListbox}
    onkeydown={onListboxKey}
  >
    {#each queue as segment, index (segment.id)}
      <li
        id={optionId(index)}
        class="rail-item"
        class:active={index === currentIndex}
        class:done={!!segment.humanDecision}
        role="option"
        tabindex="-1"
        style="border-left-color:{bandColor(segment)}"
        aria-selected={index === currentIndex}
        aria-label={optionLabel(segment, index)}
        onclick={() => onSelect(index)}
        onkeydown={(event) => onOptionKey(event, index)}
      >
        <span class="rail-id" aria-hidden="true">{segment.id.slice(0, 8)}…</span>
        {#if segment.humanDecision}
          <span class="rail-done" aria-hidden="true">{$t('inbox.reviewed')}</span>
        {/if}
      </li>
    {/each}
  </ul>
  <div class="queue-pagination" aria-live="polite">
    <p>
      {$t('inbox.pagination.loaded', {
        loaded: String(queue.length),
        total: String(queueTotal),
      })}
    </p>
    {#if nextCursor}
      <button
        type="button"
        class="btn btn-secondary btn-sm"
        onclick={onLoadMore}
        disabled={isLoadingMore}
        aria-describedby={loadMoreError ? 'inbox-load-more-error' : undefined}
      >
        {isLoadingMore ? $t('inbox.pagination.loadingMore') : $t('inbox.pagination.loadMore')}
      </button>
    {/if}
    {#if loadMoreError}
      <p id="inbox-load-more-error" class="pagination-error" role="alert">{loadMoreError}</p>
    {/if}
    {#if evictedCount > 0}
      <p class="pagination-notice" data-testid="inbox-eviction-notice">
        {$t('inbox.pagination.evicted', { count: String(evictedCount) })}
      </p>
      <button type="button" class="btn btn-secondary btn-sm" onclick={onReloadStart}>
        {$t('inbox.pagination.reloadStart')}
      </button>
    {/if}
  </div>
</nav>
<span
  class="sr-only"
  role="status"
  aria-live="polite"
  aria-atomic="true"
  data-testid="inbox-active-announcement">{activeQueueAnnouncement}</span
>

<style>
  .queue-rail {
    width: 140px;
    flex-shrink: 0;
    background: var(--surface-1);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .rail-header {
    padding: 8px 10px;
    font-size: 0.65rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    border-bottom: 1px solid var(--border);
  }
  .rail-list {
    flex: 1;
    overflow-y: auto;
    list-style: none;
    margin: 0;
    padding: 4px 0;
  }
  .rail-list:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .rail-item {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    cursor: pointer;
    transition: background 0.1s;
    font-size: 0.72rem;
    color: var(--text-muted);
    border-radius: 4px;
    margin: 1px 4px;
    user-select: none;
    width: calc(100% - 8px);
    border: 0;
    border-left: 3px solid transparent;
    background: transparent;
    text-align: left;
  }
  .rail-item:hover {
    background: var(--surface-3);
  }
  .rail-item.active {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .rail-item:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .rail-item.done {
    opacity: 0.45;
  }
  .rail-id {
    flex: 1;
    font-family: var(--font-mono);
    font-size: 0.65rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rail-done {
    color: var(--success);
    font-size: 0.55rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .queue-pagination {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 6px;
    padding: 8px;
    border-top: 1px solid var(--border);
    font-size: 0.65rem;
    color: var(--text-muted);
  }
  .queue-pagination p {
    margin: 0;
  }
  .pagination-error {
    color: var(--danger);
  }
  .pagination-notice {
    color: var(--warning);
  }

  @media (max-width: 480px) {
    .queue-rail {
      width: 100%;
      max-height: 12rem;
      border-right: 0;
      border-bottom: 1px solid var(--border);
    }
    .rail-list {
      display: flex;
      flex: 0 0 auto;
      overflow-x: auto;
      overflow-y: hidden;
      padding: 4px;
    }
    .rail-item {
      flex: 0 0 7rem;
      min-width: 0;
      width: calc(100% - 4px);
      margin: 1px 2px;
    }
    .queue-pagination {
      flex: 0 0 auto;
    }
  }
</style>
