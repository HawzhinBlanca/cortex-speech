<script lang="ts">
  import * as api from './commands';
  import { t, type TranslationKey } from './i18n';
  import ReviewDraftRecovery from './ReviewDraftRecovery.svelte';
  import type { ReviewModeDraftController } from './reviewModeDraft.svelte';
  import type { ReviewModeWordEditor } from './reviewModeWordEditor.svelte';
  import ReviewWordStrip from './ReviewWordStrip.svelte';
  import type { SpeechSegment, WordTimestamp } from './types';

  interface Props {
    current: SpeechSegment;
    editText: string;
    originalText: string;
    dirty: boolean;
    draftModels: readonly string[];
    words: WordTimestamp[];
    activeWordIndex: number;
    mutationBlocked?: boolean;
    mutationBlockedKey?: TranslationKey | null;
    draft: ReviewModeDraftController;
    wordEditor: ReviewModeWordEditor;
    onEdit: (text: string) => void;
    onReset: () => void;
    onRetryDraft: () => void;
  }

  let {
    editText,
    originalText,
    dirty,
    draftModels,
    words,
    activeWordIndex,
    mutationBlocked = false,
    mutationBlockedKey = null,
    draft,
    wordEditor,
    onEdit,
    onReset,
    onRetryDraft,
  }: Props = $props();
  const draftState = $derived(draft.state);
  const wordState = $derived(wordEditor.state);
</script>

<div class="review-transcript-card card p-5">
  {#if mutationBlocked}
    <span id="review-edit-disabled-reason" class="sr-only">
      {$t(mutationBlockedKey ?? 'inbox.disabled.saving')}
    </span>
  {/if}
  <div class="flex items-center justify-between gap-3">
    <div>
      <label
        for="review-transcript-editor"
        class="text-xs font-semibold uppercase tracking-wider text-muted">{$t('transcript')}</label
      >
      <p class="mt-0.5 text-xs text-subtle">{$t('review.editHint')}</p>
      {#if draftModels.length > 0}
        <p class="mt-1 text-[11px] text-subtle" dir="ltr">
          {$t('review.draftBy')}
          <span class="font-medium text-muted">
            {draftModels.map((model) => api.engineLabel(model)).join(', ')}
          </span>
          {$t('review.notHumanVerified')}
        </p>
      {/if}
    </div>
    {#if dirty}
      <button
        type="button"
        class="ring-focus shrink-0 rounded-token px-2 py-1 text-xs text-subtle transition-colors hover:text-default"
        onclick={onReset}
        aria-describedby={mutationBlocked ? 'review-edit-disabled-reason' : undefined}
        disabled={mutationBlocked}>{$t('review.reset')}</button
      >
    {/if}
  </div>
  <ReviewDraftRecovery
    conflict={draftState.conflict}
    serverText={originalText}
    loadFailed={draftState.loadError !== null}
    saving={draftState.saving}
    saveFailed={draftState.saveFailed}
    recovered={draftState.recovered}
    disabled={mutationBlocked}
    disabledDescriptionId="review-edit-disabled-reason"
    onUseConflict={draft.useConflict}
    onDiscardConflict={() => void draft.discardConflict()}
    onRetryLoad={onRetryDraft}
  />
  <textarea
    id="review-transcript-editor"
    value={editText}
    oninput={(event) => {
      if (!mutationBlocked) onEdit(event.currentTarget.value);
    }}
    disabled={mutationBlocked}
    aria-describedby={mutationBlocked ? 'review-edit-disabled-reason' : undefined}
    bind:this={wordState.editElement}
    dir="rtl"
    lang="ckb"
    spellcheck="false"
    class="review-transcript-input input font-kurdish mt-3 min-h-[150px] w-full resize-none text-2xl leading-loose"
    placeholder={$t('editTranscript')}
  ></textarea>
</div>

{#if words.length > 0}
  <ReviewWordStrip
    {words}
    {activeWordIndex}
    editingWordIndex={wordState.editingIndex}
    chipText={wordEditor.chipText}
    confidenceClass={wordEditor.confidenceClass}
    isEdited={(wordIndex) => !!wordState.editedChips[wordIndex]}
    editingDisabled={mutationBlocked}
    editingDisabledDescriptionId="review-edit-disabled-reason"
    onReplay={wordEditor.replay}
    onPlay={wordEditor.playWord}
    onStartEdit={wordEditor.startWordEdit}
    onCommitEdit={wordEditor.commitWordEdit}
    onCancelEdit={wordEditor.cancelWordEdit}
  />
{/if}
