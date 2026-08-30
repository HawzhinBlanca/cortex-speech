<script lang="ts">
  import AudioPlayer from './AudioPlayer.svelte';
  import type { TechnicalUnusableReasonV1 } from './commands';
  import { chunkPlaybackRange, parseSourceMeta } from './alignment';
  import { t } from './i18n';
  import type { ReviewInboxDecisionController } from './reviewInboxDecisions.svelte';
  import type { ReviewInboxDraftController } from './reviewInboxDraft.svelte';
  import type { ReviewPlaybackController } from './reviewModePlayback.svelte';
  import type { SpeechSegment } from './types';

  interface Props {
    current: SpeechSegment;
    revision: number | undefined;
    autoplay: boolean;
    playback: ReviewPlaybackController;
    decisions: ReviewInboxDecisionController;
    draft: ReviewInboxDraftController;
    mutationBlocked?: boolean;
  }

  let {
    current,
    revision,
    autoplay,
    playback,
    decisions,
    draft,
    mutationBlocked = false,
  }: Props = $props();
  const range = $derived(chunkPlaybackRange(parseSourceMeta(current.alignmentJson)));
  const playbackState = $derived(playback.state);
  const decisionState = $derived(decisions.state);
  const truthBlockedKey = $derived(decisions.newTruthDisabledKey());
  const reasons: readonly TechnicalUnusableReasonV1[] = [
    'decodeFailed',
    'missingFile',
    'permissionDenied',
    'corruptContainer',
  ];
</script>

<div class="waveform-zone" dir="ltr" aria-label={$t('inbox.audioPlayback')}>
  {#if mutationBlocked}
    <span id="inbox-playback-disabled-reason" class="sr-only">
      {$t(truthBlockedKey ?? 'inbox.disabled.saving')}
    </span>
  {/if}
  {#if current.audioPath}
    {#key `${current.id}\0${String(revision)}`}
      <AudioPlayer
        bind:this={playbackState.player}
        audioPath={current.audioPath}
        clipKey={current.id}
        startTime={range.startTime}
        endTime={range.endTime}
        {autoplay}
        bind:playing={playbackState.playing}
        bind:currentTime={playbackState.currentTime}
        bind:heardMs={playbackState.heardMs}
        bind:duration={playbackState.playerDuration}
        bind:playbackReceiptId={playbackState.playbackReceiptId}
        bind:playbackMediaGrantId={playbackState.playbackMediaGrantId}
        bind:playbackClipDurationMs={playbackState.playbackClipDurationMs}
        bind:heardIntervals={playbackState.heardIntervals}
        bind:audioError={playbackState.audioError}
        requirePlaybackProof={true}
        expectedRevision={revision}
        disabled={mutationBlocked}
        disabledDescriptionId="inbox-playback-disabled-reason"
      />
    {/key}
    {#if playbackState.audioError}
      <fieldset class="technical-unusable" data-testid="inbox-technical-unusable" dir="auto">
        <legend>{$t('review.unusable.title')}</legend>
        <p id="inbox-unusable-help">{$t('review.unusable.help')}</p>
        {#if truthBlockedKey}
          <span id="inbox-unusable-truth-reason" class="sr-only">{$t(truthBlockedKey)}</span>
        {/if}
        <label for="inbox-unusable-reason">
          {$t('review.unusable.reasonLabel')}
          <select
            id="inbox-unusable-reason"
            bind:value={decisionState.technicalReason}
            disabled={decisionState.submitting || truthBlockedKey !== null}
            aria-describedby={truthBlockedKey
              ? 'inbox-unusable-help inbox-unusable-truth-reason'
              : 'inbox-unusable-help'}
          >
            <option value="">{$t('review.unusable.reasonPlaceholder')}</option>
            {#each reasons as reason}
              <option value={reason}>{$t(`review.unusable.reason.${reason}`)}</option>
            {/each}
          </select>
        </label>
        <button
          type="button"
          class="btn btn-secondary"
          onclick={() => void decisions.markTechnicallyUnusable()}
          disabled={decisionState.submitting ||
            truthBlockedKey !== null ||
            draft.state.editing ||
            !!draft.blockedKey() ||
            !decisionState.technicalReason}
          aria-describedby={truthBlockedKey
            ? 'inbox-unusable-help inbox-unusable-truth-reason'
            : 'inbox-unusable-help'}
        >
          {decisionState.submitting ? $t('review.unusable.marking') : $t('review.unusable.mark')}
        </button>
      </fieldset>
    {/if}
    <div class="waveform-stub">
      <bdi>{current.audioPath.split(/[/\\]/).pop() ?? $t('inbox.audioFallback')}</bdi>
    </div>
  {:else}
    <div class="waveform-stub"><bdi>{$t('inbox.noAudio')}</bdi></div>
  {/if}
</div>

<style>
  .waveform-zone {
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface-2);
    padding: 12px 16px;
    color: var(--text-muted);
    font-size: 0.8rem;
  }
  .waveform-stub {
    font-size: 0.8rem;
  }
  .technical-unusable {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: 8px;
    margin-top: 10px;
    border: 1px solid color-mix(in srgb, var(--warning) 45%, transparent);
    border-radius: 8px;
    background: color-mix(in srgb, var(--warning) 10%, transparent);
    padding: 10px;
    color: var(--text);
  }
  .technical-unusable legend {
    padding-inline: 4px;
    color: var(--warning);
    font-weight: 600;
  }
  .technical-unusable p {
    flex: 1 0 100%;
    margin: 0;
    color: var(--text-muted);
  }
  .technical-unusable label {
    display: grid;
    flex: 1 1 220px;
    gap: 4px;
  }
  .technical-unusable select {
    min-width: 0;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface-1);
    padding: 6px 8px;
    color: var(--text);
  }
</style>
