<script lang="ts">
  import { t } from './i18n';
  import ReviewInboxActionBar from './ReviewInboxActionBar.svelte';
  import type { ReviewInboxDecisionController } from './reviewInboxDecisions.svelte';
  import type { ReviewInboxDraftController } from './reviewInboxDraft.svelte';

  interface Props {
    draft: ReviewInboxDraftController;
    decisions: ReviewInboxDecisionController;
    status: string;
    mutationBlocked?: boolean;
  }

  let { draft, decisions, status, mutationBlocked = false }: Props = $props();
  const draftState = $derived(draft.state);
  const decisionState = $derived(decisions.state);
  const keys = $derived(decisions.actionKeys());
</script>

{#if draftState.loadError}
  <div class="draft-state draft-error" role="alert">
    <p>{draftState.loadError}</p>
    <button
      type="button"
      class="btn btn-secondary"
      onclick={() => {
        if (!mutationBlocked) void draft.retryLoad();
      }}
      disabled={mutationBlocked}
    >
      {$t('inbox.draft.retry')}
    </button>
  </div>
{:else if draftState.conflict}
  <div class="draft-state draft-conflict" role="alert">
    <h3>{$t('review.draftConflictTitle')}</h3>
    <p>{$t('review.draftConflictHint')}</p>
    <div class="draft-comparison">
      <section>
        <h4>{$t('review.serverTruth')}</h4>
        <p dir="rtl" lang="ckb">{draftState.baseline}</p>
      </section>
      <section>
        <h4>{$t('review.localDraft')}</h4>
        <time dir="ltr">{draftState.conflict.updatedAt}</time>
        <p dir="rtl" lang="ckb">{draftState.conflict.text}</p>
      </section>
    </div>
    <div class="edit-actions">
      <button
        type="button"
        class="btn btn-primary"
        onclick={() => {
          if (!mutationBlocked) draft.useConflict();
        }}
        disabled={mutationBlocked}
      >
        {$t('review.useLocalDraft')}
      </button>
      <button
        type="button"
        class="btn btn-secondary"
        onclick={() => {
          if (!mutationBlocked) void draft.discardConflict();
        }}
        disabled={mutationBlocked}
      >
        {$t('review.discardLocalDraft')}
      </button>
    </div>
  </div>
{/if}

<div class="draft-status" aria-live="polite">
  {#if draftState.saving}
    {$t('review.draftSaving')}
  {:else if draftState.saveFailed}
    <span class="draft-error-text">{$t('review.draftSaveFailedHint')}</span>
  {:else if draftState.recovered}
    {$t('review.draftRecovered')}
  {/if}
</div>

{#if draftState.editing}
  <div class="edit-area">
    <label class="edit-label" for="edit-textarea">{$t('inbox.editLabel')}</label>
    <textarea
      id="edit-textarea"
      class="edit-textarea"
      dir="rtl"
      lang="ckb"
      value={draftState.editText}
      bind:this={draftState.textarea}
      oninput={(event) => {
        if (!mutationBlocked) draft.handleInput(event.currentTarget.value);
      }}
      disabled={decisionState.submitting || mutationBlocked}
      rows={3}
    ></textarea>
    <div class="edit-actions">
      <button
        class="btn btn-primary"
        onclick={() => void decisions.commitEdit()}
        disabled={!!keys.saveEdit}
        aria-describedby={keys.saveEdit ? 'inbox-save-edit-disabled-reason' : undefined}
        >{$t('inbox.saveEdit')}</button
      >
      {#if keys.saveEdit}
        <span id="inbox-save-edit-disabled-reason" class="sr-only">{$t(keys.saveEdit)}</span>
      {/if}
      <button
        class="btn btn-secondary"
        onclick={() => {
          if (!mutationBlocked) void draft.cancelEdit();
        }}
        disabled={mutationBlocked}
      >
        {$t('inbox.cancelEdit')}
      </button>
    </div>
  </div>
{/if}

<ReviewInboxActionBar
  acceptDisabledKey={keys.accept}
  editDisabledKey={keys.edit}
  rejectDisabledKey={keys.reject}
  skipDisabledKey={keys.skip}
  flagDisabledKey={keys.flag}
  undoDisabledKey={keys.undo}
  undoActionKey={decisions.undoActionKey()}
  undoErrorCode={decisions.undoErrorCode()}
  onAccept={() => void decisions.accept()}
  onEdit={() => void draft.startEdit()}
  onReject={() => void decisions.reject()}
  onSkip={() => void decisions.skip()}
  onFlag={() => void decisions.flag()}
  onUndo={() => void decisions.undo()}
/>

{#if status}<div class="status-bar" role="status" aria-live="polite">{status}</div>{/if}

<style>
  .draft-error-text {
    color: var(--danger);
  }
  .draft-state {
    display: grid;
    gap: 8px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface-2);
    padding: 12px;
    color: var(--text-muted);
    font-size: 0.78rem;
  }
  .draft-state h3,
  .draft-state h4,
  .draft-state p {
    margin: 0;
  }
  .draft-error {
    border-color: color-mix(in srgb, var(--danger) 45%, transparent);
  }
  .draft-conflict {
    border-color: color-mix(in srgb, var(--warning) 45%, transparent);
  }
  .draft-comparison {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px;
  }
  .draft-comparison section {
    min-width: 0;
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px;
  }
  .draft-comparison p {
    margin-top: 6px;
    white-space: pre-wrap;
  }
  .draft-status {
    min-height: 1.2em;
    color: var(--text-muted);
    font-size: 0.72rem;
  }
  .edit-area {
    display: flex;
    flex-direction: column;
    gap: 8px;
    border: 1px solid var(--accent);
    border-radius: 8px;
    background: var(--surface-2);
    padding: 12px;
  }
  .edit-label {
    color: var(--accent);
    font-size: 0.72rem;
    font-weight: 600;
  }
  .edit-textarea {
    width: 100%;
    resize: vertical;
    border: 1px solid var(--border);
    border-radius: 6px;
    outline: none;
    background: var(--surface-1);
    padding: 10px 14px;
    color: var(--text);
    font-family: var(--font-kurdish);
    font-size: 1.1rem;
    line-height: 1.9;
  }
  .edit-textarea:focus {
    border-color: var(--accent);
  }
  .edit-actions {
    display: flex;
    gap: 8px;
  }
  .status-bar {
    animation: fadeIn 0.2s ease;
    border-radius: 4px;
    background: var(--surface-2);
    padding: 6px 10px;
    color: var(--text-muted);
    font-size: 0.72rem;
  }
  @keyframes fadeIn {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }
  @media (max-width: 480px) {
    .draft-comparison {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
