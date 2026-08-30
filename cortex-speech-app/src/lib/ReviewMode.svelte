<script lang="ts">
  import { onDestroy, onMount, tick } from 'svelte';
  import { segments, selectedSegmentId, searchQuery } from './stores/segmentStore';
  import * as api from './commands';
  import { notifications } from './stores/notificationStore';
  import { settings } from './stores/settingsStore';
  import { showReviewInbox, showConfirmDialog, isProcessing } from './stores/uiStore';
  import { t } from './i18n';
  import ReviewModeActive from './ReviewModeActive.svelte';
  import ReviewModeTerminal from './ReviewModeTerminal.svelte';
  import {
    parseWordTimestamps,
    parseSourceMeta,
    chunkPlaybackRange,
    segmentChunkLabel,
  } from './alignment';
  import { revealReviewCompletion } from './reviewCompletion';
  import { parseEscalationEvidence } from './reasonCodes';
  import { registerReviewDraftFlusher } from './reviewDraftFlush';
  import { createReviewModeQueueController } from './reviewModeQueue.svelte';
  import { createReviewModeWordEditor } from './reviewModeWordEditor.svelte';
  import { createReviewModeDraftController } from './reviewModeDraft.svelte';
  import { createReviewModePlaybackController } from './reviewModePlayback.svelte';
  import { createReviewModeDecisionController } from './reviewModeDecisions.svelte';
  import { handleReviewModeKeydown } from './reviewModeKeyboard';
  import { reviewTranscript } from './reviewTranscriptAuthority';
  import type { SpeechSegment, WordTimestamp } from './types';

  interface Props {
    // Pro next-steps surfaced when the whole queue is reviewed (wired by App to its export / exit).
    onExport?: () => void;
    onDone?: () => void;
  }
  let { onExport, onDone }: Props = $props();

  const reviewQueue = createReviewModeQueueController();
  // Every async continuation that can publish renderer/global state is owned by this component
  // instance. Destruction retires the epoch before unregistering projections, so an old GPU/read
  // completion cannot write through a replacement Review surface.
  let lifecycleEpoch = 0;
  let disposed = false;
  const lifecycleCurrent = (epoch: number) => !disposed && epoch === lifecycleEpoch;
  const queueState = reviewQueue.state;
  const reviewRows = $derived(queueState.rows);
  const reviewRevisions = $derived(queueState.revisions);
  const reviewTotal = $derived(queueState.total);
  const reviewInitialTotal = $derived(queueState.initialTotal);
  const reviewLoading = $derived(queueState.loading);
  const reviewLoadError = $derived(queueState.loadError);
  const focusNarrowed = $derived(queueState.focusNarrowed);
  const current = $derived(reviewQueue.current());
  const searchScoped = $derived(reviewQueue.searchScoped());
  const progress = $derived(reviewQueue.progress());

  $effect(() => {
    const nextSearch = $searchQuery;
    // The Library search remains mounted beside Review. Defer its scope intent while a truth lease
    // owns the exact visible clip; otherwise typing in that field can reset this queue underneath a
    // held/ambiguous decision just as surely as the suspect-first toggle can.
    if (!decisionController.editMutationBlocked()) reviewQueue.syncScope(nextSearch);
  });

  const loadReviewPage = reviewQueue.load;
  const hydrateReviewRow = reviewQueue.hydrate;

  $effect(() => {
    reviewQueue.restoreCursor($selectedSegmentId);
  });

  $effect(() => {
    reviewQueue.hydrateCandidate();
  });

  $effect(() => {
    reviewQueue.maybeLoadMore();
  });
  let reviewScroller = $state<HTMLDivElement | null>(null);
  let wasComplete = false;
  $effect(() => {
    const completedNow = progress.allReviewed;
    if (completedNow && !wasComplete) {
      void tick().then(() => {
        wasComplete = revealReviewCompletion(reviewScroller, completedNow, wasComplete);
      });
    } else {
      wasComplete = completedNow;
    }
  });
  // Null whenever the current clip carries no decision record — never escalated, or decided before the
  // codes existed. The template renders nothing in that case rather than asserting "no reasons".
  const escalationReasons = $derived(parseEscalationEvidence(current?.evidenceJson));
  // Hoisted so the label is computed once instead of twice per render, and so the chunk position gets
  // a NOUN in the markup below: bare "61/144" beside "Clip 1 of 144" read as a third progress counter.
  const chunkLabel = $derived(current ? segmentChunkLabel(current.alignmentJson) : null);

  let editText = $state('');
  const playbackController = createReviewModePlaybackController({
    inboxOpen: () => $showReviewInbox,
  });
  const playbackState = playbackController.state;

  // Show the engines actually recorded for this draft. The retired multi-model consensus changed
  // 0/135 measured owner clips and could downgrade champion text; only its honest provenance list
  // remains.
  let draftModels = $state<string[]>([]);
  let consensusSeq = 0;
  async function loadConsensus(seg: SpeechSegment) {
    const seq = ++consensusSeq;
    draftModels = [];
    try {
      const c = await api.getSegmentConsensus(seg.id);
      if (seq !== consensusSeq) return;
      draftModels = c.models ?? [];
    } catch {
      if (seq === consensusSeq) {
        draftModels = [];
      }
    }
  }

  // Re-transcribe this clip only through the production champion. The backend commits the new text
  // and exact model/cloud provenance atomically; this UI only reloads that authoritative row.
  let retranscribing = $state(false);
  function retranscribe() {
    const seg = current;
    if (disposed || !seg || retranscribing || saving || aligning || $isProcessing) return;
    // Human review is authoritative. Reopen/undo it first; an ASR request never doubles as an implicit
    // destructive undo, even behind a confirmation dialog that could become stale during inference.
    if (seg.verified || seg.humanDecision) {
      notifications.info($t('asr.reopenBeforeRetranscribe'));
      return;
    }
    void doRetranscribe();
  }
  async function doRetranscribe() {
    const seg = current;
    if (disposed || !seg || retranscribing || saving || aligning || $isProcessing) return;
    const attemptEpoch = lifecycleEpoch;
    retranscribing = true;
    try {
      await api.transcribeSegment(seg.audioPath, seg.alignmentJson, seg.id);
      if (!lifecycleCurrent(attemptEpoch)) return;
      // The champion command commits server-side after all enabled refinement succeeds. Reload that
      // authoritative row; never spread/upsert the UI snapshot captured before a long GPU call.
      const updated = await api.getSegment(seg.id);
      if (!lifecycleCurrent(attemptEpoch)) return;
      const text = originalText(updated);
      segments.update((list) => list.map((s) => (s.id === seg.id ? updated : s)));
      queueState.rows = reviewRows.map((s) => (s.id === seg.id ? updated : s));
      notifications.success($t('review.retranscribed'));
      // The DB/store write above targets seg by id and is correct even if the reviewer navigated away
      // during the multi-second ASR await. But everything below mutates the CURRENTLY shown editor
      // (editText/lastLoadedOriginal/draftModels/alignment) — if navigation changed `current`
      // mid-flight, applying seg's draft here would put seg's MACHINE text into another clip's editor,
      // and a subsequent Save would persist it as THAT clip's human-verified gold: a wrong-segment gold
      // corruption (THE ONE LAW). Bail; seg's clip reloads its fresh draft + re-aligns when reopened.
      if (current?.id !== seg.id) return;
      editText = text;
      wordEditor.resetChips();
      // The re-transcribed draft is the new baseline (not a "dirty" edit). Do NOT reset lastLoadedId —
      // the clip id is unchanged, so the load effect must stay a no-op; resetting it would re-run
      // loadConsensus and wipe the provenance badge we set just below.
      draftController.setBaseline(text);
      // Use the provenance committed by the backend, never a UI-side model assumption. Automatic
      // alignment (or stale-word removal) is part of that same transaction, so do not issue a second
      // alignment mutation after the exact Undo endpoint has been recorded.
      draftModels = updated.modelVersionId ? [updated.modelVersionId] : [];
      if ($settings.autoAlign) alignAttempted.add(seg.id);
    } catch (e) {
      if (!lifecycleCurrent(attemptEpoch)) return;
      // Champion down: fail closed and retry only the champion. There is no in-flow downgrade.
      if (api.is7bUnavailableError(e)) {
        showConfirmDialog.set({
          title: $t('asr.championUnavailableTitle'),
          message: $t('asr.championUnavailableMessage'),
          confirmLabel: $t('asr.tryAgain'),
          danger: false,
          onConfirm: () => void doRetranscribe(),
        });
      } else {
        notifications.error($t('review.retranscribeFailed'), { cause: e });
      }
    } finally {
      if (lifecycleCurrent(attemptEpoch)) retranscribing = false;
    }
  }

  // Cloud watcher: one Gemini-2.5-Pro listen on THIS clip (the T2 judge, on demand). Gemini hears the
  // audio and checks the draft verbatim — including repeated words ("کە کە") the local ASR often
  // collapses — and returns a corrected transcript + reason. The verdict renders inline; the reviewer
  // stays the decider (a "use this text" click only fills the editor, never auto-verifies).
  let cloudChecking = $state(false);
  let cloudCheck = $state<{ id: string; result: import('./commands').T2Result } | null>(null);
  async function runCloudCheck() {
    const seg = current;
    if (disposed || !seg || cloudChecking || saving || retranscribing) return;
    const attemptEpoch = lifecycleEpoch;
    cloudChecking = true;
    cloudCheck = null;
    try {
      const result = await api.runT2ForSegment(seg.id, $settings.llmApiKey);
      if (!lifecycleCurrent(attemptEpoch)) return;
      cloudCheck = { id: seg.id, result };
    } catch (e) {
      if (!lifecycleCurrent(attemptEpoch)) return;
      notifications.error($t('review.cloudCheckFailed'), { cause: e });
    } finally {
      if (lifecycleCurrent(attemptEpoch)) cloudChecking = false;
    }
  }

  // Word-level alignment for the current clip (forced or heuristic). When present it
  // powers the listen-strip: tap a word to hear it, colour the low-confidence ones so
  // the reviewer's eye lands on likely errors, and karaoke-highlight the active word.
  const words = $derived<WordTimestamp[]>(parseWordTimestamps(current?.alignmentJson));

  // The clip's window within its source file. Without this the player plays the WHOLE file
  // and the waveform playhead is whole-file-relative — so you don't hear/see the one sentence
  // you're correcting. Bound the player to [start,end] and show the waveform clip-relative.
  const range = $derived(chunkPlaybackRange(parseSourceMeta(current?.alignmentJson)));
  // Word timestamps from the aligner are CLIP-relative (0-based within the chunk), so compare
  // against currentTime minus the clip's start offset; otherwise an offset chunk never highlights.
  const activeWordIndex = $derived.by(() => {
    const clipT = playbackState.currentTime - range.startTime;
    return words.findIndex((w) => clipT >= w.start && clipT < w.end);
  });
  const clipLength = $derived(
    range.endTime > range.startTime
      ? range.endTime - range.startTime
      : playbackState.playerDuration,
  );
  const clipPosition = $derived(
    range.endTime > range.startTime
      ? Math.max(0, Math.min(playbackState.currentTime - range.startTime, clipLength))
      : playbackState.currentTime,
  );

  // The primary decision playback covers the complete database segment. Word chips can temporarily
  // narrow playback, but accepting a clip requires hearing the same full source span the backend
  // hashes and evaluates; silently skipping VAD padding could make an honest 85% receipt impossible
  // or hide a defect in the omitted audio.
  const playStart = $derived(range.startTime);
  const wordEditor = createReviewModeWordEditor({
    words: () => words,
    range: () => range,
    editText: () => editText,
    setEditText: (text) => (editText = text),
    playing: () => playbackState.playing,
    setPlaying: (next) => (playbackState.playing = next),
    setCurrentTime: (time) => (playbackState.currentTime = time),
    playStart: () => playStart,
    mutationBlocked: () => decisionController.editMutationBlocked(),
  });
  const wordState = wordEditor.state;
  const clearWordOverride = wordEditor.clearOverride;
  const replay = wordEditor.replay;

  // Lazily compute + PERSIST forced-alignment word timings the first time a clip is opened without
  // them, so the spoken-span playback + tap-a-word strip light up for the whole existing backlog
  // (alignment reuses the saved transcript + audio; it does NOT re-run ASR). Best-effort: review still
  // works with whole-clip playback if alignment is unavailable.
  let aligning = $state(false);
  const alignAttempted = new Set<string>();
  async function ensureWordTimings(seg: SpeechSegment) {
    // Alignment can load a separate CTC/MMS runtime. It is an explicit optional operation, not work
    // that opening a review clip may launch behind the owner's back (especially while champion GPUs
    // are occupied). Factory default is off; the existing timings/whole-clip playback remain valid.
    if (!$settings.autoAlign) return;
    // Re-align when timings are MISSING or still the energy heuristic (evenly spaced words that do
    // not track the voice): imported clips always carry heuristic timings, so gating on "has
    // timestamps" alone froze the entire backlog at heuristic quality even after a real CTC aligner
    // was installed. Idempotent: without an aligner the backend re-persists the same heuristic, and
    // alignAttempted stops per-session repeats either way.
    const hasRealTimings =
      parseWordTimestamps(seg.alignmentJson).length > 0 &&
      seg.alignmentQuality !== 'energy_heuristic';
    if (hasRealTimings || alignAttempted.has(seg.id)) return;
    const text = originalText(seg);
    if (!text.trim() || text.includes('[Pending') || text.includes('[ASR unavailable')) return;
    alignAttempted.add(seg.id);
    aligning = true;
    try {
      await api.alignSegment(seg.audioPath, text, seg.alignmentJson ?? null, seg.id);
      await hydrateReviewRow(seg.id, true);
    } catch {
      // Best-effort alignment, but still refresh the authoritative row: a CAS refusal means another
      // writer changed its chunk metadata while inference ran, and the review surface must follow it.
      try {
        await hydrateReviewRow(seg.id, true);
      } catch {
        // The existing full row remains safe and reviewable if even the refresh is unavailable.
      }
    } finally {
      aligning = false;
    }
  }

  function originalText(seg: SpeechSegment): string {
    // Verbatim Law: human-authored text is authoritative; otherwise review the immutable champion
    // hypothesis. Refined/normalized machine text remains labeled evidence and never becomes the
    // reviewer-facing transcript merely because it is more fluent.
    return reviewTranscript(seg);
  }

  const draftController = createReviewModeDraftController({
    current: () => current,
    editText: () => editText,
    setEditText: (text) => (editText = text),
    originalText,
    onSelectionActivated: (segment) => {
      playbackState.currentTime = 0;
      playbackState.playing = false;
      wordState.editingIndex = null;
      wordEditor.resetChips();
      void playbackController.loadWaveform(segment);
      void ensureWordTimings(segment);
      void loadConsensus(segment);
    },
  });
  $effect(() => {
    const seg = current;
    const revision = seg ? (reviewRevisions[seg.id] ?? null) : null;
    if (seg) draftController.activateSelection(seg, revision);
  });

  $effect(() => {
    return draftController.scheduleActiveWrite($showReviewInbox ? null : current);
  });

  const flushActiveReviewDraft = draftController.flush;

  const unregisterDraftFlusher = registerReviewDraftFlusher(flushActiveReviewDraft);
  onDestroy(() => {
    disposed = true;
    ++lifecycleEpoch;
    ++consensusSeq;
    reviewQueue.dispose();
    decisionController.disposeUndoProjection();
    // Native close awaits this same barrier from App before destruction. This fallback covers
    // ordinary component navigation; it cannot make teardown await, so errors remain visible through
    // the registered close path and the durable draft status already rendered in this workspace.
    void flushActiveReviewDraft().catch(() => undefined);
    unregisterDraftFlusher();
  });

  async function retryReviewDraftLoad() {
    const seg = current;
    const revision = seg ? reviewRevisions[seg.id] : undefined;
    await draftController.retry(revision);
  }
  const dirty = $derived(current ? editText.trim() !== originalText(current).trim() : false);
  const decisionController = createReviewModeDecisionController({
    queue: reviewQueue,
    draft: draftController,
    playback: playbackController,
    editText: () => editText,
    setEditText: (text) => (editText = text),
    originalText,
    dirty: () => dirty,
    retranscribing: () => retranscribing,
    aligning: () => aligning,
    resetWords: wordEditor.resetChips,
  });
  reviewQueue.setNavigationBlocked(decisionController.editMutationBlocked);
  const saving = $derived(decisionController.state.saving);
  const editMutationBlocked = $derived(decisionController.editMutationBlocked());
  $effect(() => {
    if (editMutationBlocked) {
      wordState.editingIndex = null;
      playbackState.playing = false;
    }
  });
  const submit = decisionController.submit;
  const markBad = decisionController.markBad;
  const undoLast = decisionController.undoLast;
  const go = decisionController.go;

  onMount(() => {
    void decisionController.refreshUndo();
  });

  function resetToOriginal() {
    const segment = current;
    const revision = segment ? reviewRevisions[segment.id] : undefined;
    const expectedText = editText;
    const confirmationEpoch = lifecycleEpoch;
    if (disposed || editMutationBlocked || !segment || !Number.isSafeInteger(revision) || !dirty)
      return;
    showConfirmDialog.set({
      title: $t('review.resetConfirmTitle'),
      message: $t('review.resetConfirmMessage'),
      confirmLabel: $t('review.resetConfirmAction'),
      danger: true,
      onConfirm: async () => {
        if (
          !lifecycleCurrent(confirmationEpoch) ||
          current?.id !== segment.id ||
          reviewRevisions[segment.id] !== revision ||
          editText !== expectedText ||
          editMutationBlocked
        )
          return;
        try {
          if (
            await draftController.discardActiveEdit(segment.id, revision as number, expectedText)
          ) {
            wordEditor.resetChips();
          }
        } catch (error) {
          notifications.error($t('review.draftDiscardFailed'), { cause: error });
        }
      },
    });
  }

  $effect(() => {
    if (!playbackState.playing) clearWordOverride();
  });

  function onKeydown(event: KeyboardEvent) {
    handleReviewModeKeydown(event, {
      inboxOpen: () => $showReviewInbox,
      submit: (acceptAsIs) => void submit(acceptAsIs),
      focusEditor: () => wordState.editElement?.focus(),
      blurEditor: () => wordState.editElement?.blur(),
      markBad: () => void markBad(),
      togglePlayback: () => {
        if (!editMutationBlocked) playbackState.playing = !playbackState.playing;
      },
      replay: () => {
        if (!editMutationBlocked) replay();
      },
      navigate: (delta) => void go(delta),
      undo: () => void undoLast(),
    });
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if reviewLoading && reviewInitialTotal === 0}
  <ReviewModeTerminal mode="loading" onRetry={() => void loadReviewPage(true)} />
{:else if reviewLoadError && !current}
  <ReviewModeTerminal
    mode="error"
    error={reviewLoadError}
    onRetry={() => void loadReviewPage(true)}
  />
{:else if !current && reviewTotal > 0}
  <ReviewModeTerminal mode="loading" onRetry={() => void loadReviewPage(true)} />
{:else if !current}
  <ReviewModeTerminal
    mode="complete"
    {searchScoped}
    {focusNarrowed}
    allReviewed={progress.allReviewed}
    onRetry={() => void loadReviewPage(true)}
    {onExport}
    {onDone}
  />
{:else}
  <ReviewModeActive
    {current}
    queue={reviewQueue}
    draft={draftController}
    playback={playbackController}
    decisions={decisionController}
    {wordEditor}
    {editText}
    originalText={originalText(current)}
    {dirty}
    {range}
    {clipPosition}
    {clipLength}
    {words}
    {activeWordIndex}
    {aligning}
    autoplay={$settings.autoplaySegments}
    inboxOpen={$showReviewInbox}
    {draftModels}
    {retranscribing}
    cloudOptIn={$settings.juryCloudOptIn}
    {cloudChecking}
    {cloudCheck}
    {escalationReasons}
    {chunkLabel}
    bind:scroller={reviewScroller}
    onEdit={(text) => {
      if (!editMutationBlocked) editText = text;
    }}
    onReset={resetToOriginal}
    onRetryDraft={() => void retryReviewDraftLoad()}
    onRetranscribe={retranscribe}
    onCloudCheck={() => void runCloudCheck()}
    {onExport}
    {onDone}
  />
{/if}
