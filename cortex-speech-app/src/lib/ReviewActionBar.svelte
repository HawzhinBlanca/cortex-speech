<script lang="ts">
  import { t, type TranslationKey } from './i18n';

  interface Props {
    eligibilityBlocked: boolean;
    eligibilityReason: string;
    audioUnavailable: boolean;
    draftBlockedKey: TranslationKey | null;
    dirty: boolean;
    saving: boolean;
    retranscribing: boolean;
    previousDisabled: boolean;
    undoDisabledKey: TranslationKey | null;
    undoActionKey: TranslationKey;
    undoErrorCode: string | null;
    truthBlockedKey: TranslationKey | null;
    decisionBlocked: boolean;
    editHasText: boolean;
    onPrevious: () => void;
    onUndo: () => void;
    onReject: () => void;
    onAccept: () => void;
    onSave: () => void;
  }

  let {
    eligibilityBlocked,
    eligibilityReason,
    audioUnavailable,
    draftBlockedKey,
    dirty,
    saving,
    retranscribing,
    previousDisabled,
    undoDisabledKey,
    undoActionKey,
    undoErrorCode,
    truthBlockedKey,
    decisionBlocked,
    editHasText,
    onPrevious,
    onUndo,
    onReject,
    onAccept,
    onSave,
  }: Props = $props();
  const draftBlocked = $derived(draftBlockedKey !== null);
</script>

<div
  class="review-action-bar flex flex-wrap items-center gap-2"
  role="group"
  aria-label={$t('review.actionsLabel')}
  data-testid="review-action-bar"
>
  {#if eligibilityBlocked}<span id="review-eligibility-disabled-reason" class="sr-only"
      >{eligibilityReason}</span
    >{/if}
  {#if audioUnavailable}<span id="review-audio-disabled-reason" class="sr-only"
      >{$t('review.cannotDecideWithoutAudio')}</span
    >{/if}
  {#if draftBlockedKey}<span id="review-draft-disabled-reason" class="sr-only"
      >{$t(draftBlockedKey)}</span
    >{/if}
  {#if dirty}<span id="review-reject-disabled-reason" class="sr-only"
      >{$t('review.rejectDisabledEdited')}</span
    >{/if}
  {#if undoDisabledKey}<span id="review-undo-disabled-reason" class="sr-only"
      >{$t(undoDisabledKey)}</span
    >{/if}
  {#if truthBlockedKey}<span id="review-truth-disabled-reason" class="sr-only"
      >{$t(truthBlockedKey)}</span
    >{/if}
  {#if undoErrorCode}<span class="text-xs text-amber-300" role="status"
      >{$t('review.undoErrorCode', { code: undoErrorCode })}</span
    >{/if}

  <button
    type="button"
    class="btn btn-secondary"
    onclick={onPrevious}
    disabled={previousDisabled}
    aria-label={$t('prevSegment')}
  >
    {$t('review.prev')}
  </button>
  <button
    type="button"
    class="btn btn-secondary"
    onclick={onUndo}
    disabled={!!undoDisabledKey}
    aria-describedby={undoDisabledKey ? 'review-undo-disabled-reason' : undefined}
    title={$t(undoActionKey)}
  >
    {$t(undoActionKey)}
  </button>
  <button
    type="button"
    class="btn btn-secondary !text-rose-300 hover:!text-rose-200"
    onclick={onReject}
    disabled={saving || retranscribing || decisionBlocked || dirty}
    aria-describedby={eligibilityBlocked
      ? 'review-eligibility-disabled-reason'
      : audioUnavailable
        ? 'review-audio-disabled-reason'
        : truthBlockedKey
          ? 'review-truth-disabled-reason'
          : draftBlocked
            ? 'review-draft-disabled-reason'
            : dirty
              ? 'review-reject-disabled-reason'
              : undefined}
    title={$t('review.markBadTitle')}
  >
    {$t('review.markBad')}
  </button>
  <div class="review-primary-actions flex flex-1 flex-wrap justify-end gap-2">
    <button
      type="button"
      class="btn btn-secondary !py-2.5"
      onclick={onAccept}
      disabled={saving || dirty || decisionBlocked}
      aria-describedby={eligibilityBlocked
        ? 'review-eligibility-disabled-reason'
        : audioUnavailable
          ? 'review-audio-disabled-reason'
          : truthBlockedKey
            ? 'review-truth-disabled-reason'
            : dirty
              ? 'review-accept-disabled-reason'
              : draftBlocked
                ? 'review-draft-disabled-reason'
                : undefined}
    >
      {$t('review.acceptAsIs')}
    </button>
    {#if dirty}<span id="review-accept-disabled-reason" class="sr-only"
        >{$t('review.acceptDisabledEdited')}</span
      >{/if}
    <button
      type="button"
      class="btn btn-primary !py-2.5 !text-sm"
      onclick={onSave}
      disabled={saving || !editHasText || decisionBlocked}
      aria-describedby={eligibilityBlocked
        ? 'review-eligibility-disabled-reason'
        : audioUnavailable
          ? 'review-audio-disabled-reason'
          : truthBlockedKey
            ? 'review-truth-disabled-reason'
            : draftBlocked
              ? 'review-draft-disabled-reason'
              : undefined}
    >
      {$t('review.saveNext')}
    </button>
  </div>
  <p class="w-full text-center text-[11px] text-subtle">{$t('review.kbdHint')}</p>
</div>

<style>
  .review-action-bar {
    position: sticky;
    bottom: 0;
    z-index: 5;
    flex: none;
    background: var(--surface-1);
    border-top: 1px solid var(--border);
    padding: 8px 1rem max(8px, env(safe-area-inset-bottom));
  }
  @media (min-width: 800px) and (max-height: 700px) {
    .review-action-bar > p {
      display: none;
    }
  }
  @media (max-width: 599px) {
    .review-action-bar {
      display: grid;
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
      gap: 0.375rem;
      padding: 0.375rem 0.5rem max(0.375rem, env(safe-area-inset-bottom));
    }
    .review-action-bar > button,
    .review-primary-actions > button {
      width: 100%;
      min-width: 0;
      padding: 0.375rem 0.5rem !important;
      font-size: 0.75rem !important;
    }
    .review-primary-actions {
      display: contents;
    }
    .review-action-bar > p {
      display: none;
    }
  }
</style>
