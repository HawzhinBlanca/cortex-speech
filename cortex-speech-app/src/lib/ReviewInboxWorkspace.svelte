<script lang="ts">
  import { t } from './i18n';
  import type { ReviewInboxDecisionController } from './reviewInboxDecisions.svelte';
  import type { ReviewInboxDraftController } from './reviewInboxDraft.svelte';
  import ReviewInboxFocus from './ReviewInboxFocus.svelte';
  import ReviewInboxHeader from './ReviewInboxHeader.svelte';
  import type { ReviewInboxQueueController } from './reviewInboxQueue.svelte';
  import ReviewInboxQueueRail from './ReviewInboxQueueRail.svelte';
  import type { ReviewInboxRuntimeController } from './reviewInboxRuntime.svelte';
  import type { ReviewPlaybackController } from './reviewModePlayback.svelte';
  import { confidenceBand } from './reviewLabels';

  interface Props {
    queue: ReviewInboxQueueController;
    draft: ReviewInboxDraftController;
    decisions: ReviewInboxDecisionController;
    playback: ReviewPlaybackController;
    runtime: ReviewInboxRuntimeController;
  }

  let { queue, draft, decisions, playback, runtime }: Props = $props();
  const queueState = $derived(queue.state);
  const runtimeState = $derived(runtime.state);
  const runJuryDisabledKey = $derived(runtime.juryDisabledKey());
  const mutationBlocked = $derived(decisions.editMutationBlocked());
  const current = $derived(queue.current());
  const currentRevision = $derived(queue.currentRevision());
  const poorAudio = (segment: { snrDb?: number | null; clippingRatio?: number | null }) =>
    (segment.snrDb != null && segment.snrDb < 5) ||
    (segment.clippingRatio != null && segment.clippingRatio > 0.1);
</script>

<div class="inbox-root" role="dialog" aria-modal="true" aria-labelledby="review-inbox-title">
  <ReviewInboxHeader
    pendingCount={queue.pendingCount()}
    isRunningJury={runtimeState.juryRunning}
    {runJuryDisabledKey}
    localOnly={!!runtimeState.settings && !runtimeState.settings.juryCloudOptIn}
    autonomyLevel={runtimeState.autonomyLevel}
    closePending={runtimeState.closePending}
    onRunJury={() => void runtime.runJury()}
    onSetAutonomy={(level) => void runtime.setAutonomy(level)}
    onClose={() => void runtime.requestClose()}
  />
  {#if queueState.loading}
    <div class="inbox-loading"><span class="spinner"></span>{$t('inbox.loadingQueue')}</div>
  {:else if queueState.loadError}
    <div class="inbox-empty" role="alert" data-testid="review-inbox-load-error">
      <h3>{$t('inbox.loadErrorTitle')}</h3>
      <p>{queueState.loadError}</p>
      <button class="btn btn-primary" onclick={() => void queue.load()}>{$t('inbox.retry')}</button>
    </div>
  {:else if queueState.rows.length === 0}
    <div class="inbox-empty">
      <h3>{$t('inbox.zero')}</h3>
      <p>{$t('inbox.zeroHint')}</p>
      <div class="empty-actions">
        <button
          class="btn btn-primary"
          onclick={() => void runtime.runJury()}
          disabled={!!runJuryDisabledKey}
          aria-describedby={runJuryDisabledKey ? 'inbox-empty-jury-disabled-reason' : undefined}
        >
          {runtimeState.juryRunning ? $t('inbox.runningJury') : $t('inbox.runJuryPipeline')}
        </button>
        {#if runJuryDisabledKey}
          <span id="inbox-empty-jury-disabled-reason" class="sr-only">
            {$t(runJuryDisabledKey)}
          </span>
        {/if}
        <button
          class="btn btn-secondary"
          onclick={() => void queue.load()}
          disabled={mutationBlocked}
        >
          {$t('inbox.refresh')}
        </button>
      </div>
    </div>
  {:else}
    <div class="inbox-body">
      {#if mutationBlocked}
        <span id="inbox-navigation-disabled-reason" class="sr-only">
          {$t(decisions.newTruthDisabledKey() ?? 'inbox.disabled.saving')}
        </span>
      {/if}
      <ReviewInboxQueueRail
        queue={queueState.rows}
        currentIndex={queueState.index}
        {current}
        queueTotal={queueState.total}
        nextCursor={queueState.nextCursor}
        isLoadingMore={queueState.loadingMore}
        loadMoreError={queueState.loadMoreError}
        evictedCount={queueState.evictedCount}
        activeQueueAnnouncement={queue.activeAnnouncement()}
        bind:queueListbox={queueState.listbox}
        bandColor={(segment) =>
          confidenceBand(segment.agreementScore, $t, poorAudio(segment)).color}
        optionId={queue.optionId}
        optionLabel={queue.optionLabel}
        onListboxKey={queue.handleListboxKey}
        onOptionKey={queue.handleOptionKey}
        onSelect={(index) => void queue.select(index, true, true)}
        onLoadMore={() => void queue.loadMore()}
        onReloadStart={() => void queue.load()}
        navigationDisabled={mutationBlocked}
        navigationDisabledDescriptionId="inbox-navigation-disabled-reason"
      />
      {#if current}
        <ReviewInboxFocus
          {current}
          revision={currentRevision}
          autoplay={runtimeState.settings?.autoplaySegments ?? false}
          status={runtimeState.status}
          {playback}
          {draft}
          {decisions}
          {mutationBlocked}
        />
      {/if}
    </div>
  {/if}
</div>

<style>
  .inbox-root {
    display: flex;
    position: fixed;
    z-index: 100;
    inset: 0;
    height: 100%;
    flex-direction: column;
    overflow: hidden;
    background: var(--surface-1);
    color: var(--text);
  }
  .inbox-loading,
  .inbox-empty {
    display: flex;
    flex: 1;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    padding: 24px;
    text-align: center;
  }
  .inbox-empty h3 {
    margin: 0;
    color: var(--accent);
    font-size: 1.2rem;
  }
  .inbox-empty p {
    max-width: 400px;
    margin: 0;
    color: var(--text-muted);
  }
  .empty-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 8px;
    width: 100%;
    max-width: 28rem;
  }
  .spinner {
    width: 18px;
    height: 18px;
    animation: spin 0.7s linear infinite;
    border: 2px solid var(--accent);
    border-top-color: transparent;
    border-radius: 50%;
  }
  .inbox-body {
    display: flex;
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (max-width: 480px) {
    .inbox-root {
      overflow-y: auto;
    }
    .inbox-body {
      flex: 1 1 auto;
      min-height: auto;
      flex-direction: column;
      overflow: visible;
    }
  }
</style>
