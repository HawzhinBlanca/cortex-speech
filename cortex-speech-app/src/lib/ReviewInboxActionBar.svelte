<script lang="ts">
  import { t, type TranslationKey } from './i18n';

  interface Props {
    acceptDisabledKey: TranslationKey | null;
    editDisabledKey: TranslationKey | null;
    rejectDisabledKey: TranslationKey | null;
    skipDisabledKey: TranslationKey | null;
    flagDisabledKey: TranslationKey | null;
    undoDisabledKey: TranslationKey | null;
    undoActionKey: TranslationKey;
    undoErrorCode: string | null;
    onAccept: () => void;
    onEdit: () => void;
    onReject: () => void;
    onSkip: () => void;
    onFlag: () => void;
    onUndo: () => void;
  }

  let {
    acceptDisabledKey,
    editDisabledKey,
    rejectDisabledKey,
    skipDisabledKey,
    flagDisabledKey,
    undoDisabledKey,
    undoActionKey,
    undoErrorCode,
    onAccept,
    onEdit,
    onReject,
    onSkip,
    onFlag,
    onUndo,
  }: Props = $props();
</script>

<div class="verb-bar" role="group" aria-label={$t('inbox.reviewActions')}>
  {#if undoErrorCode}<span class="undo-error" role="status"
      >{$t('review.undoErrorCode', { code: undoErrorCode })}</span
    >{/if}
  <button
    class="verb-btn accept"
    onclick={onAccept}
    title={$t('inbox.acceptTitle')}
    id="inbox-accept"
    disabled={!!acceptDisabledKey}
    aria-describedby={acceptDisabledKey ? 'inbox-accept-disabled-reason' : undefined}
  >
    <span class="verb-key">A</span>
    {$t('inbox.accept')}
  </button>
  {#if acceptDisabledKey}
    <span id="inbox-accept-disabled-reason" class="sr-only">{$t(acceptDisabledKey)}</span>
  {/if}
  <button
    class="verb-btn edit"
    onclick={onEdit}
    title={$t('inbox.editTitle')}
    id="inbox-edit"
    disabled={!!editDisabledKey}
    aria-describedby={editDisabledKey ? 'inbox-edit-disabled-reason' : undefined}
  >
    <span class="verb-key">E</span>
    {$t('inbox.edit')}
  </button>
  {#if editDisabledKey}
    <span id="inbox-edit-disabled-reason" class="sr-only">{$t(editDisabledKey)}</span>
  {/if}
  <button
    class="verb-btn reject"
    onclick={onReject}
    title={$t('inbox.rejectTitle')}
    id="inbox-reject"
    disabled={!!rejectDisabledKey}
    aria-describedby={rejectDisabledKey ? 'inbox-reject-disabled-reason' : undefined}
  >
    <span class="verb-key">X</span>
    {$t('inbox.reject')}
  </button>
  {#if rejectDisabledKey}
    <span id="inbox-reject-disabled-reason" class="sr-only">{$t(rejectDisabledKey)}</span>
  {/if}
  <button
    class="verb-btn skip"
    onclick={onSkip}
    title={$t('inbox.skipTitle')}
    id="inbox-skip"
    disabled={!!skipDisabledKey}
    aria-describedby={skipDisabledKey ? 'inbox-skip-disabled-reason' : undefined}
  >
    <span class="verb-key">S</span>
    {$t('inbox.skip')}
  </button>
  {#if skipDisabledKey}
    <span id="inbox-skip-disabled-reason" class="sr-only">{$t(skipDisabledKey)}</span>
  {/if}
  <button
    class="verb-btn flag"
    onclick={onFlag}
    title={$t('inbox.flagTitle')}
    id="inbox-flag"
    disabled={!!flagDisabledKey}
    aria-describedby={flagDisabledKey ? 'inbox-flag-disabled-reason' : undefined}
  >
    <span class="verb-key">F</span>
    {$t('inbox.flag')}
  </button>
  {#if flagDisabledKey}
    <span id="inbox-flag-disabled-reason" class="sr-only">{$t(flagDisabledKey)}</span>
  {/if}
  <button
    class="verb-btn undo"
    onclick={onUndo}
    title={$t(undoActionKey)}
    id="inbox-undo"
    disabled={!!undoDisabledKey}
    aria-describedby={undoDisabledKey ? 'inbox-undo-disabled-reason' : undefined}
  >
    {$t(undoActionKey)}
  </button>
  {#if undoDisabledKey}
    <span id="inbox-undo-disabled-reason" class="sr-only">{$t(undoDisabledKey)}</span>
  {/if}
</div>

<style>
  .verb-bar {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    background: var(--surface-1);
    border-top: 1px solid var(--border);
    padding: 12px 0 4px;
    position: sticky;
    bottom: 0;
  }
  .undo-error {
    width: 100%;
    color: rgb(var(--amber-400-rgb));
    font-size: 0.75rem;
  }
  .verb-btn {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 16px;
    border-radius: 8px;
    border: 1px solid transparent;
    font-size: 0.8rem;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.15s;
    letter-spacing: 0.02em;
  }
  .verb-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .verb-key {
    background: color-mix(in srgb, currentColor 16%, transparent);
    border-radius: 4px;
    padding: 1px 5px;
    font-family: var(--font-mono);
    font-size: 0.7rem;
  }
  .verb-btn.accept {
    background: rgb(var(--emerald-500-rgb) / 0.12);
    border-color: rgb(var(--emerald-500-rgb) / 0.35);
    color: rgb(var(--emerald-400-rgb));
  }
  .verb-btn.accept:hover:not(:disabled) {
    background: rgb(var(--emerald-500-rgb) / 0.2);
  }
  .verb-btn.edit {
    background: rgb(var(--blue-500-rgb) / 0.12);
    border-color: rgb(var(--blue-500-rgb) / 0.35);
    color: rgb(var(--blue-400-rgb));
  }
  .verb-btn.edit:hover:not(:disabled) {
    background: rgb(var(--blue-500-rgb) / 0.2);
  }
  .verb-btn.reject {
    background: rgb(var(--red-500-rgb) / 0.12);
    border-color: rgb(var(--red-500-rgb) / 0.35);
    color: rgb(var(--red-400-rgb));
  }
  .verb-btn.reject:hover:not(:disabled) {
    background: rgb(var(--red-500-rgb) / 0.2);
  }
  .verb-btn.skip,
  .verb-btn.undo {
    background: var(--surface-2);
    border-color: var(--border);
  }
  .verb-btn.skip {
    color: var(--text-muted);
  }
  .verb-btn.undo {
    color: var(--text-subtle);
  }
  .verb-btn.skip:hover:not(:disabled),
  .verb-btn.undo:hover:not(:disabled) {
    background: var(--surface-3);
    color: var(--text);
  }
  .verb-btn.flag {
    background: rgb(var(--amber-500-rgb) / 0.12);
    border-color: rgb(var(--amber-500-rgb) / 0.35);
    color: rgb(var(--amber-400-rgb));
  }
  .verb-btn.flag:hover:not(:disabled) {
    background: rgb(var(--amber-500-rgb) / 0.2);
  }
</style>
