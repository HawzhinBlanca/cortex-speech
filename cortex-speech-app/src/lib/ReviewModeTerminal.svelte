<script lang="ts">
  import EmptyState from './EmptyState.svelte';
  import { t } from './i18n';

  interface Props {
    mode: 'loading' | 'error' | 'complete';
    error?: string | null;
    searchScoped?: boolean;
    focusNarrowed?: boolean;
    allReviewed?: boolean;
    onRetry: () => void;
    onExport?: () => void;
    onDone?: () => void;
  }

  let {
    mode,
    error = null,
    searchScoped = false,
    focusNarrowed = false,
    allReviewed = false,
    onRetry,
    onExport,
    onDone,
  }: Props = $props();
</script>

{#if mode === 'loading'}
  <div class="flex min-h-full items-center [justify-content:safe_center] p-6" aria-busy="true">
    <div class="text-sm text-subtle">{$t('loading')}</div>
  </div>
{:else if mode === 'error'}
  <div
    class="flex min-h-full items-center [justify-content:safe_center] p-6"
    data-testid="review-load-error"
    role="alert"
  >
    <EmptyState
      variant="error"
      title={$t('notifications.loadSegmentsFailed')}
      description={error ?? $t('errors.unknown')}
    >
      <button type="button" class="btn btn-primary !text-sm" onclick={onRetry}>
        {$t('retry')}
      </button>
    </EmptyState>
  </div>
{:else}
  <div
    class="flex min-h-full items-center [justify-content:safe_center] p-6"
    data-testid="review-terminal"
  >
    <div class="flex flex-col items-center gap-4 text-center">
      <EmptyState
        variant="empty"
        title={$t('review.allDone')}
        description={searchScoped
          ? $t('review.searchScopeEmpty')
          : focusNarrowed
            ? $t('review.focusScopeEmpty')
            : $t('review.allDoneHint')}
      />
      <div class="flex flex-wrap justify-center gap-2">
        {#if allReviewed && onExport}
          <button
            type="button"
            class="btn btn-primary !text-sm"
            data-testid="review-terminal-export"
            onclick={onExport}
          >
            {$t('review.exportDataset')}
          </button>
        {/if}
        {#if onDone}
          <button
            type="button"
            class="btn btn-secondary !text-sm"
            data-testid="review-terminal-done"
            onclick={onDone}
          >
            {$t('review.backToLibrary')}
          </button>
        {/if}
      </div>
    </div>
  </div>
{/if}
