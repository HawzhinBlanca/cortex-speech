<script lang="ts">
  import { t } from './i18n';
  import type { ReviewDraftV1 } from './commands';

  interface Props {
    conflict: ReviewDraftV1 | null;
    serverText: string;
    loadFailed: boolean;
    saving: boolean;
    saveFailed: boolean;
    recovered: boolean;
    onUseConflict: () => void;
    onDiscardConflict: () => void;
    onRetryLoad: () => void;
  }

  let {
    conflict,
    serverText,
    loadFailed,
    saving,
    saveFailed,
    recovered,
    onUseConflict,
    onDiscardConflict,
    onRetryLoad,
  }: Props = $props();
</script>

{#if conflict}
  <div class="mt-3 rounded-token border border-warning/40 bg-warning/10 p-3" role="alert">
    <div class="text-sm font-semibold text-default">{$t('review.draftConflictTitle')}</div>
    <p class="mt-1 text-xs text-muted">{$t('review.draftConflictHint')}</p>
    <div class="mt-3 grid gap-3 md:grid-cols-2">
      <section class="rounded-token bg-surface-raised p-3">
        <div class="text-xs font-semibold text-muted">{$t('review.serverTruth')}</div>
        <p class="font-kurdish mt-1 whitespace-pre-wrap text-base" dir="rtl">{serverText}</p>
      </section>
      <section class="rounded-token bg-surface-raised p-3">
        <div class="flex flex-wrap items-center justify-between gap-2 text-xs text-muted">
          <span class="font-semibold">{$t('review.localDraft')}</span>
          <time dir="ltr">{conflict.updatedAt}</time>
        </div>
        <p class="font-kurdish mt-1 whitespace-pre-wrap text-base" dir="rtl">{conflict.text}</p>
      </section>
    </div>
    <div class="mt-3 flex flex-wrap gap-2">
      <button type="button" class="btn btn-primary !text-xs" onclick={onUseConflict}>
        {$t('review.useLocalDraft')}
      </button>
      <button type="button" class="btn btn-secondary !text-xs" onclick={onDiscardConflict}>
        {$t('review.discardLocalDraft')}
      </button>
    </div>
  </div>
{/if}

<div class="mt-2 min-h-5 text-xs text-muted" aria-live="polite">
  {#if loadFailed}
    <span class="text-danger">{$t('review.draftLoadFailedHint')}</span>
    <button
      type="button"
      class="ring-focus ms-2 rounded-token px-2 py-1 text-xs text-cortex-300 hover:text-default"
      onclick={onRetryLoad}
    >
      {$t('retry')}
    </button>
  {:else if saving}
    {$t('review.draftSaving')}
  {:else if saveFailed}
    <span class="text-danger">{$t('review.draftSaveFailedHint')}</span>
  {:else if recovered}
    {$t('review.draftRecovered')}
  {/if}
</div>
