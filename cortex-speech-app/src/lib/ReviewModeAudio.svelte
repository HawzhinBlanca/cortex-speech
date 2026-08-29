<script lang="ts">
  import AudioPlayer from './AudioPlayer.svelte';
  import Waveform from './Waveform.svelte';
  import type { TechnicalUnusableReasonV1 } from './commands';
  import { t } from './i18n';
  import type { ReviewModeDecisionController } from './reviewModeDecisions.svelte';
  import type { ReviewModePlaybackController } from './reviewModePlayback.svelte';
  import type { ReviewModeWordEditor } from './reviewModeWordEditor.svelte';
  import type { SpeechSegment, WordTimestamp } from './types';

  interface Props {
    current: SpeechSegment;
    revision: number;
    range: { startTime: number; endTime: number };
    clipPosition: number;
    clipLength: number;
    words: WordTimestamp[];
    aligning: boolean;
    autoplay: boolean;
    inboxOpen: boolean;
    dirty: boolean;
    draftBlocked: boolean;
    mutationBlocked: boolean;
    playback: ReviewModePlaybackController;
    decisions: ReviewModeDecisionController;
    wordEditor: ReviewModeWordEditor;
  }

  let {
    current,
    revision,
    range,
    clipPosition,
    clipLength,
    words,
    aligning,
    autoplay,
    inboxOpen,
    dirty,
    draftBlocked,
    mutationBlocked,
    playback,
    decisions,
    wordEditor,
  }: Props = $props();
  const playbackState = $derived(playback.state);
  const decisionState = $derived(decisions.state);
  const wordState = $derived(wordEditor.state);
  const truthBlockedKey = $derived(
    mutationBlocked ? (decisions.newTruthDisabledKey() ?? 'inbox.disabled.saving') : null,
  );
  const reasons: readonly TechnicalUnusableReasonV1[] = [
    'decodeFailed',
    'missingFile',
    'permissionDenied',
    'corruptContainer',
  ];
</script>

<div class="review-audio-card card overflow-hidden">
  {#if truthBlockedKey}
    <span id="review-playback-disabled-reason" class="sr-only">{$t(truthBlockedKey)}</span>
  {/if}
  <div class="overflow-hidden">
    {#if playbackState.waveformError}
      <div
        class="flex items-center justify-between gap-3 p-3 text-xs text-amber-300"
        data-testid="review-waveform-error"
        role="status"
      >
        <span class="min-w-0 truncate">{$t('review.waveformFailed')}</span>
        <button
          type="button"
          class="btn btn-secondary shrink-0 !text-xs"
          onclick={() => {
            if (!mutationBlocked) void playback.loadWaveform(current);
          }}
          aria-describedby={mutationBlocked ? 'review-playback-disabled-reason' : undefined}
          disabled={mutationBlocked}>{$t('retry')}</button
        >
      </div>
    {:else}
      <Waveform
        waveform={playbackState.waveformData}
        currentTime={clipPosition}
        duration={clipLength}
        playing={playbackState.playing}
        wordTimestamps={words}
        disabled={mutationBlocked}
        disabledDescriptionId="review-playback-disabled-reason"
        onSeek={(time) => {
          if (mutationBlocked) return;
          wordEditor.clearOverride();
          playbackState.currentTime = range.startTime + time;
        }}
      />
    {/if}
  </div>
  <div
    class="flex items-center gap-2 border-t border-subtle px-3 py-2 text-xs text-subtle"
    aria-live="polite"
  >
    {#if aligning}
      <span
        class="inline-block h-3 w-3 animate-spin rounded-full border-2 border-accent border-t-transparent"
      ></span>
      <span>{$t('review.aligningWords')}</span>
    {:else}
      <span>{$t('review.playingWholeClip').replace('{sec}', clipLength.toFixed(1))}</span>
    {/if}
  </div>
  {#if !inboxOpen}
    {#key `${current.id}\0${String(revision)}`}
      <AudioPlayer
        bind:this={playbackState.player}
        audioPath={current.audioPath}
        clipKey={current.id}
        startTime={wordState.startOverride ?? range.startTime}
        endTime={wordState.endOverride ?? range.endTime}
        displayStart={range.startTime}
        displayEnd={range.endTime}
        evidenceStart={range.startTime}
        evidenceEnd={range.endTime}
        bind:currentTime={playbackState.currentTime}
        bind:duration={playbackState.playerDuration}
        bind:playing={playbackState.playing}
        bind:audioError={playbackState.audioError}
        bind:heardMs={playbackState.heardMs}
        bind:playbackReceiptId={playbackState.playbackReceiptId}
        bind:playbackMediaGrantId={playbackState.playbackMediaGrantId}
        bind:playbackClipDurationMs={playbackState.playbackClipDurationMs}
        bind:heardIntervals={playbackState.heardIntervals}
        {autoplay}
        requirePlaybackProof={true}
        expectedRevision={revision}
        disabled={mutationBlocked}
        disabledDescriptionId="review-playback-disabled-reason"
      />
    {/key}
  {/if}
  {#if playbackState.audioError}
    <fieldset
      class="m-3 mt-0 rounded-lg border border-amber-500/40 bg-amber-500/10 p-3"
      data-testid="review-technical-unusable"
    >
      <legend class="px-1 text-xs font-semibold text-amber-200">
        {$t('review.unusable.title')}
      </legend>
      <p id="review-unusable-help" class="mb-2 text-xs text-subtle">
        {$t('review.unusable.help')}
      </p>
      <div class="flex flex-wrap items-end gap-2">
        <label class="min-w-48 flex-1 text-xs text-muted" for="review-unusable-reason">
          <span class="mb-1 block">{$t('review.unusable.reasonLabel')}</span>
          <select
            id="review-unusable-reason"
            class="input w-full"
            bind:value={decisionState.technicalUnusableReason}
            disabled={mutationBlocked}
            aria-describedby="review-unusable-help"
          >
            <option value="">{$t('review.unusable.reasonPlaceholder')}</option>
            {#each reasons as reason}
              <option value={reason}>{$t(`review.unusable.reason.${reason}`)}</option>
            {/each}
          </select>
        </label>
        <button
          type="button"
          class="btn btn-secondary !text-amber-100"
          onclick={() => void decisions.markTechnicallyUnusable()}
          disabled={mutationBlocked ||
            !decisionState.technicalUnusableReason ||
            draftBlocked ||
            dirty}
          aria-describedby={draftBlocked
            ? 'review-unusable-help review-draft-disabled-reason'
            : dirty
              ? 'review-unusable-help review-reject-disabled-reason'
              : 'review-unusable-help'}
        >
          {decisionState.saving ? $t('review.unusable.marking') : $t('review.unusable.mark')}
        </button>
      </div>
    </fieldset>
  {/if}
</div>
