<script lang="ts">
  import type { T2Result } from './commands';
  import ReviewActionBar from './ReviewActionBar.svelte';
  import ReviewModeAudio from './ReviewModeAudio.svelte';
  import type { ReviewModeDecisionController } from './reviewModeDecisions.svelte';
  import type { ReviewModeDraftController } from './reviewModeDraft.svelte';
  import type { ReviewModePlaybackController } from './reviewModePlayback.svelte';
  import ReviewModeProgress from './ReviewModeProgress.svelte';
  import type { ReviewModeQueueController } from './reviewModeQueue.svelte';
  import ReviewModeTools from './ReviewModeTools.svelte';
  import ReviewModeTranscript from './ReviewModeTranscript.svelte';
  import type { ReviewModeWordEditor } from './reviewModeWordEditor.svelte';
  import type { EscalationEvidence } from './reasonCodes';
  import type { SpeechSegment, WordTimestamp } from './types';

  interface Props {
    current: SpeechSegment;
    queue: ReviewModeQueueController;
    draft: ReviewModeDraftController;
    playback: ReviewModePlaybackController;
    decisions: ReviewModeDecisionController;
    wordEditor: ReviewModeWordEditor;
    editText: string;
    originalText: string;
    dirty: boolean;
    range: { startTime: number; endTime: number };
    clipPosition: number;
    clipLength: number;
    words: WordTimestamp[];
    activeWordIndex: number;
    aligning: boolean;
    autoplay: boolean;
    inboxOpen: boolean;
    draftModels: readonly string[];
    retranscribing: boolean;
    cloudOptIn: boolean;
    cloudChecking: boolean;
    cloudCheck: { id: string; result: T2Result } | null;
    escalationReasons: EscalationEvidence | null;
    chunkLabel: string | null;
    scroller?: HTMLDivElement | null;
    onEdit: (text: string) => void;
    onReset: () => void;
    onRetryDraft: () => void;
    onRetranscribe: () => void;
    onCloudCheck: () => void;
    onExport?: () => void;
    onDone?: () => void;
  }

  let {
    current,
    queue,
    draft,
    playback,
    decisions,
    wordEditor,
    editText,
    originalText,
    dirty,
    range,
    clipPosition,
    clipLength,
    words,
    activeWordIndex,
    aligning,
    autoplay,
    inboxOpen,
    draftModels,
    retranscribing,
    cloudOptIn,
    cloudChecking,
    cloudCheck,
    escalationReasons,
    chunkLabel,
    scroller = $bindable(null),
    onEdit,
    onReset,
    onRetryDraft,
    onRetranscribe,
    onCloudCheck,
    onExport,
    onDone,
  }: Props = $props();
  const queueState = $derived(queue.state);
  const decisionState = $derived(decisions.state);
  const progress = $derived(queue.progress());
  const eligibility = $derived(queue.currentEligibility());
  const eligibilityBlocked = $derived(eligibility?.eligible !== true);
  const draftBlockedKey = $derived(draft.blockedKey());
  const draftBlocked = $derived(draftBlockedKey !== null);
  const truthBlockedKey = $derived(decisions.newTruthDisabledKey());
  const mutationBlocked = $derived(decisions.editMutationBlocked());
  const scopeMutationBlockedKey = $derived(
    mutationBlocked ? (truthBlockedKey ?? 'inbox.disabled.saving') : null,
  );
  const decisionBlocked = $derived(
    eligibilityBlocked ||
      playback.state.audioError !== null ||
      draftBlocked ||
      truthBlockedKey !== null,
  );
</script>

<div class="flex h-full min-h-0 flex-col">
  <div class="min-h-0 flex-1 overflow-y-auto" bind:this={scroller}>
    <div class="review-stack mx-auto flex max-w-3xl flex-col gap-5 px-4 py-6">
      <ReviewModeProgress
        {current}
        {progress}
        queueLength={queue.queue().length}
        index={queueState.index}
        corpusTotal={queueState.corpusTotal}
        subsetScoped={queue.subsetScoped()}
        searchScoped={queue.searchScoped()}
        suspectFirst={queueState.suspectFirst}
        suspectToggleDisabled={mutationBlocked}
        suspectToggleDisabledKey={scopeMutationBlockedKey}
        {escalationReasons}
        {chunkLabel}
        onToggleSuspect={() => {
          // A held/ambiguous truth operation owns the exact clip, draft, player, and focus. Scope
          // changes reset the queue, so fence the handler as well as disabling the control.
          if (!decisions.editMutationBlocked()) queue.toggleSuspectFirst();
        }}
        onReviewAgain={() => (queueState.index = 0)}
        {onExport}
        {onDone}
      />
      <ReviewModeAudio
        {current}
        revision={queueState.revisions[current.id]}
        {range}
        {clipPosition}
        {clipLength}
        {words}
        {aligning}
        {autoplay}
        {inboxOpen}
        {dirty}
        {draftBlocked}
        {mutationBlocked}
        {playback}
        {decisions}
        {wordEditor}
      />
      <ReviewModeTranscript
        {current}
        {editText}
        {originalText}
        {dirty}
        {draftModels}
        {words}
        {activeWordIndex}
        {mutationBlocked}
        mutationBlockedKey={scopeMutationBlockedKey}
        {draft}
        {wordEditor}
        {onEdit}
        {onReset}
        {onRetryDraft}
      />
      <ReviewModeTools
        currentId={current.id}
        {editText}
        {cloudOptIn}
        saving={decisionState.saving}
        {mutationBlocked}
        {retranscribing}
        {cloudChecking}
        {cloudCheck}
        {onRetranscribe}
        {onCloudCheck}
        {onEdit}
      />
    </div>
  </div>
  <ReviewActionBar
    {eligibilityBlocked}
    eligibilityReason={decisions.eligibilityReasonText(eligibility?.disabledReason)}
    audioUnavailable={playback.state.audioError !== null}
    {draftBlockedKey}
    {dirty}
    saving={decisionState.saving}
    {retranscribing}
    previousDisabled={queueState.index === 0}
    undoDisabledKey={decisions.undoDisabledKey()}
    undoActionKey={decisions.undoActionKey()}
    undoErrorCode={decisions.undoErrorCode()}
    {truthBlockedKey}
    {decisionBlocked}
    editHasText={editText.trim().length > 0}
    onPrevious={() => void decisions.go(-1)}
    onUndo={() => void decisions.undoLast()}
    onReject={() => void decisions.markBad()}
    onAccept={() => void decisions.submit(true)}
    onSave={() => void decisions.submit(false)}
  />
</div>

<style>
  @media (min-width: 800px) and (max-height: 700px) {
    .review-stack {
      display: grid;
      max-width: none;
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
      align-content: start;
      gap: 0.5rem;
      padding: 0.5rem 1rem;
    }
    .review-stack :global(.review-wide),
    .review-stack :global(.review-progress),
    .review-stack :global(.review-secondary) {
      grid-column: 1 / -1;
    }
    .review-stack :global(.review-transcript-card) {
      padding: 0.75rem;
    }
    .review-stack :global(.review-transcript-input) {
      min-height: 100px;
      font-size: 1.125rem;
      line-height: 1.75rem;
    }
  }
</style>
