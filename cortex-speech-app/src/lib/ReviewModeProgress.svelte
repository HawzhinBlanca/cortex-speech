<script lang="ts">
  import { segmentSourceFilename } from './alignment';
  import { t, type TranslationKey } from './i18n';
  import { reasonLabelKey, reasonTone, type EscalationEvidence } from './reasonCodes';
  import type { SpeechSegment } from './types';

  interface Props {
    current: SpeechSegment;
    progress: { done: number; total: number; percent: number; allReviewed: boolean };
    queueLength: number;
    index: number;
    corpusTotal: number;
    subsetScoped: boolean;
    searchScoped: boolean;
    suspectFirst: boolean;
    suspectToggleDisabled: boolean;
    suspectToggleDisabledKey: TranslationKey | null;
    escalationReasons: EscalationEvidence | null;
    chunkLabel: string | null;
    onToggleSuspect: () => void;
    onReviewAgain: () => void;
    onExport?: () => void;
    onDone?: () => void;
  }

  let {
    current,
    progress,
    queueLength,
    index,
    corpusTotal,
    subsetScoped,
    searchScoped,
    suspectFirst,
    suspectToggleDisabled,
    suspectToggleDisabledKey,
    escalationReasons,
    chunkLabel,
    onToggleSuspect,
    onReviewAgain,
    onExport,
    onDone,
  }: Props = $props();
</script>

{#if progress.allReviewed}
  <div
    class="review-wide card border border-emerald-700/40 bg-emerald-950/20 p-5 text-center"
    data-testid="review-complete"
  >
    <div class="text-lg font-semibold text-emerald-300">
      {$t('review.completeTitle').replace('{n}', String(corpusTotal))}
    </div>
    <p class="mt-1 text-sm text-subtle">{$t('review.completeHint')}</p>
    <div class="mt-4 flex flex-wrap justify-center gap-2">
      {#if onExport}
        <button
          type="button"
          class="btn btn-primary !text-sm"
          data-testid="review-complete-export"
          onclick={onExport}>{$t('review.exportDataset')}</button
        >
      {/if}
      <button type="button" class="btn btn-secondary !text-sm" onclick={onReviewAgain}>
        {$t('review.reviewAgain')}
      </button>
      {#if onDone}
        <button type="button" class="btn btn-secondary !text-sm" onclick={onDone}>
          {$t('review.backToLibrary')}
        </button>
      {/if}
    </div>
  </div>
{/if}

{#if subsetScoped}
  <div
    class="review-wide rounded-lg border border-amber-600/40 bg-amber-950/30 px-3 py-2 text-xs text-amber-300"
    data-testid="review-scope-banner"
  >
    {$t(searchScoped ? 'review.searchScope' : 'review.focusScope')
      .replace('{n}', String(queueLength))
      .replace('{m}', String(corpusTotal))}
  </div>
{/if}

<div class="review-progress">
  <div class="flex items-center justify-between gap-3">
    <span class="text-sm font-medium text-muted">
      {$t('review.progress')
        .replace('{n}', String(index + 1))
        .replace('{total}', String(queueLength))}
    </span>
    <div class="flex items-center gap-2">
      <button
        type="button"
        data-testid="suspect-first-toggle"
        onclick={() => {
          if (!suspectToggleDisabled) onToggleSuspect();
        }}
        disabled={suspectToggleDisabled}
        aria-describedby={suspectToggleDisabled ? 'review-scope-disabled-reason' : undefined}
        title={suspectToggleDisabledKey
          ? $t(suspectToggleDisabledKey)
          : $t('review.suspectFirstHint')}
        aria-pressed={suspectFirst}
        class="rounded-md border px-2 py-1 text-xs transition-colors {suspectFirst
          ? 'border-accent bg-accent/15 text-accent'
          : 'border-surface-3 text-subtle hover:text-muted'}">{$t('review.suspectFirst')}</button
      >
      {#if suspectToggleDisabledKey}
        <span id="review-scope-disabled-reason" class="sr-only">
          {$t(suspectToggleDisabledKey)}
        </span>
      {/if}
      <span class="badge {current.verified ? 'badge-verified' : 'badge-pending'}">
        {current.verified ? $t('verified') : $t('pending')}
      </span>
    </div>
  </div>
  <div class="mt-2 h-1.5 overflow-hidden rounded-full bg-surface-3">
    <div
      class="h-full rounded-full bg-accent transition-all duration-300"
      style="width: {progress.percent}%"
    ></div>
  </div>
  {#if escalationReasons}
    <div class="mt-2 flex flex-wrap items-center gap-1.5" data-testid="escalation-reasons">
      <span class="text-[11px] uppercase tracking-wider text-subtle">
        {$t('reason.whyEscalated')}
      </span>
      {#each escalationReasons.reasonCodes as code (code)}
        {@const key = reasonLabelKey(code)}
        <span
          class="reason-chip reason-{reasonTone(code)}"
          title={escalationReasons.policyVersion ?? undefined}>{key ? $t(key) : code}</span
        >
      {/each}
    </div>
  {/if}
  <div class="mt-1 flex items-center justify-between gap-3 text-xs text-subtle">
    <span class="flex min-w-0 items-center gap-1.5">
      <span class="truncate" dir="ltr" title={current.audioPath} data-testid="review-source-file"
        >{segmentSourceFilename(current.audioPath)}</span
      >
      {#if chunkLabel}
        <span class="shrink-0" data-testid="review-chunk-label">
          {$t('chunk')} <span dir="ltr">{chunkLabel}</span>
        </span>
      {/if}
    </span>
    <span class="shrink-0">
      {$t('review.reviewedCount')
        .replace('{done}', String(progress.done))
        .replace('{total}', String(progress.total))}
    </span>
  </div>
</div>

<style>
  .reason-chip {
    display: inline-flex;
    align-items: center;
    border: 1px solid transparent;
    border-radius: 9999px;
    padding: 1px 8px;
    font-size: 0.6875rem;
    line-height: 1.4;
  }
  .reason-danger {
    border-color: color-mix(in srgb, var(--danger) 40%, transparent);
    background: color-mix(in srgb, var(--danger) 16%, transparent);
    color: var(--text);
  }
  .reason-warning {
    border-color: color-mix(in srgb, var(--warning) 40%, transparent);
    background: color-mix(in srgb, var(--warning) 16%, transparent);
    color: var(--text);
  }
  .reason-neutral {
    border-color: var(--border);
    background: var(--surface-3);
    color: var(--text-muted, var(--text));
  }
</style>
