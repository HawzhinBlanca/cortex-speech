<script lang="ts">
  /**
   * ReviewInbox.svelte — Phase 3: The Review Inbox
   *
   * Single-item focus card + queue rail (riskiest first) with full Prodigy
   * keyboard map: a accept · e edit · x reject · space play/pause · r replay · n/p navigate · ⌫ undo
   *
   * RTL rules: Sorani text in dir="rtl" / lang="ckb" blocks.
   * LTR exceptions: waveform, timecodes, model names (wrapped in <bdi>).
   * Confidence shown as band + icon + verb — never as a raw float.
   */

  import { onMount, onDestroy, tick } from 'svelte';
  import * as api from './commands';
  import { physicalKey } from './keyboard';
  import { autonomyLabelKey, t, type AutonomyLevel, type TranslationKey } from './i18n';
  import { parseSourceMeta, chunkPlaybackRange } from './alignment';
  import type { SpeechSegment } from './types';
  import type { AppSettings } from './stores/settingsStore';
  import { settings as settingsStore } from './stores/settingsStore';
  import AudioPlayer from './AudioPlayer.svelte';
  import ReviewInboxActionBar from './ReviewInboxActionBar.svelte';
  import ReviewInboxQueueRail from './ReviewInboxQueueRail.svelte';
  import ReviewInboxHeader from './ReviewInboxHeader.svelte';
  import { ReviewCommitOperationLedger, type ReviewCommitIntent } from './reviewCommitOperation';
  import {
    ReviewPlaybackAttemptLedger,
    hasSufficientReviewPlayback,
    isProvenUncommittedPlaybackFinalization,
  } from './reviewPlaybackAttempt';
  import { isCommittedReviewFor } from './reviewCommitResult';
  import { registerReviewDraftFlusher } from './reviewDraftFlush';
  import { ReviewDraftWriteCoordinator } from './reviewDraftWriteCoordinator';
  import { formatPublicErrorReference } from './errorText';
  import type { ReviewDraftV1, ReviewPageV1 } from './commands';
  import { confidenceBand } from './reviewLabels';

  // ── Props ───────────────────────────────────────────────────────────────────
  export let onClose: () => void = () => {};

  // ── State ───────────────────────────────────────────────────────────────────
  let queue: SpeechSegment[] = [];
  // Policy-4 review authority comes from the same typed page snapshot as each row. A raw segment
  // does not carry a revision, so retaining the legacy escalation response would make a real CAS
  // commit impossible (or tempt the renderer to guess a revision).
  let queueRevisions: Record<string, number> = {};
  let queueEligibility: Record<string, { eligible: boolean; disabledReason: string | null }> = {};
  const INBOX_PAGE_SIZE = 200;
  const MAX_RESIDENT_PAGES = 3;
  const MAX_RESIDENT_ROWS = INBOX_PAGE_SIZE * MAX_RESIDENT_PAGES;
  const NEAR_END_THRESHOLD = 10;
  let nextCursor: string | null = null;
  let queueTotal = 0;
  let isLoadingMore = false;
  let loadMoreError: string | null = null;
  let evictedCount = 0;
  let queueLoadGeneration = 0;
  let currentIndex = 0;
  let isLoading = false;
  // A transport/database failure is not an empty queue. Keep the failure as first-class state so
  // the reviewer is never shown the celebratory "Inbox zero" claim when nothing was actually read.
  let loadError: string | null = null;
  let isEditing = false;
  let editText = '';
  let editTextarea: HTMLTextAreaElement | null = null;
  let statusMsg = '';
  let queueListbox: HTMLUListElement | null = null;
  let announcedQueueIndex: number | null = null;
  type InboxHistoryEntry =
    | {
        kind: 'human';
        id: string;
        decision: 'accept' | 'edit' | 'reject';
        effectEventId: number;
        operationId: string;
      }
    | {
        kind: 'flag';
        id: string;
        decision: 'flag';
        effectEventId: number;
        operationId: string;
      };
  let history: InboxHistoryEntry[] = [];
  const commitOperations = new ReviewCommitOperationLedger();
  const playbackAttempts = new ReviewPlaybackAttemptLedger();
  // Round-23 #12: the autonomy dial reflects and WRITES the real backend `jury_autonomy_level` setting
  // (read by the T0 gate's apply_autonomy), not a dead local variable.
  let autonomyLevel: AutonomyLevel = 'propose';
  // Persisted app settings (mirrors Settings). Seeded on mount, written on a dial change so it can be
  // persisted via update_settings, and read to surface the cloud-T2 (jury) consent state in the header.
  let settings: AppSettings | null = null;
  // Keyboard play/pause state for the current clip (Space); reset on queue navigation so a new
  // clip never inherits the previous clip's playing flag.
  let inboxPlaying = false;
  let inboxCurrentTime = 0;
  // Cumulative MEDIA time heard for the clip on screen, bound from AudioPlayer. The backend
  // refuses a verdict without it, so a decision here has to carry the same proof the phone does.
  let inboxHeardMs = 0;
  let inboxDuration = 0;
  let inboxPlaybackReceiptId: string | null = null;
  let inboxPlaybackMediaGrantId: string | null = null;
  let inboxPlaybackClipDurationMs: number | null = null;
  let inboxHeardIntervals: readonly { startMs: number; endMs: number }[] = [];
  let inboxAudioPlayer:
    | {
        pauseAndSnapshot: () => {
          segmentId: string | null;
          segmentRevision: number | null;
          playbackReceiptId: string | null;
          mediaGrantId: string | null;
          clipDurationMs: number | null;
          intervals: readonly { startMs: number; endMs: number }[];
        };
        restartPlaybackAuthority: () => void;
      }
    | undefined;
  // Non-null = this clip's audio failed to load. No verdict may be recorded on audio nobody could
  // hear: in a VERBATIM corpus an unheard "looks good" is indistinguishable from a real listen
  // downstream, and worse than no decision at all (audit find 2026-08-17).
  let audioError: string | null = null;
  const technicalUnusableReasons: readonly api.TechnicalUnusableReasonV1[] = [
    'decodeFailed',
    'missingFile',
    'permissionDenied',
    'corruptContainer',
  ];
  let technicalUnusableReason: api.TechnicalUnusableReasonV1 | '' = '';
  let technicalUnusableIntent: api.MarkSegmentUnusableRequestV1 | null = null;
  let editingForId: string | null = null;
  let draftBaseline = '';
  let draftReadyId: string | null = null;
  let draftConflict: ReviewDraftV1 | null = null;
  let draftRecovered = false;
  let draftSaving = false;
  let draftSaveFailed = false;
  let draftLoadError: string | null = null;
  let draftPending = false;
  let draftLoadSequence = 0;
  let selectionGeneration = 0;
  let activeSelectionKey: string | null = null;
  let draftSaveTimer: ReturnType<typeof setTimeout> | null = null;
  const draftWrites = new ReviewDraftWriteCoordinator({
    save: api.saveReviewDraftV1,
    onStateChange: () => refreshDraftSaving(),
    onWriteSucceeded: (intent) => {
      if (
        isActiveDraftTarget(intent.segmentId, intent.baseRevision) &&
        !draftWrites.hasDesired(intent.segmentId)
      ) {
        draftSaveFailed = false;
      }
    },
    onWriteFailed: (intent) => {
      if (isActiveDraftTarget(intent.segmentId, intent.baseRevision)) {
        draftSaveFailed = true;
        statusMsg = $t('review.draftSaveFailedHint');
      }
    },
  });
  // Keep the active option visible while DOM focus remains on the single composite listbox tab stop.
  // This avoids a 200-button tab sequence without letting the active row disappear below the rail.
  $: if (currentIndex >= 0) void scrollRailToCurrent();
  async function scrollRailToCurrent() {
    await tick();
    queueListbox
      ?.querySelector<HTMLElement>('[role="option"][aria-selected="true"]')
      ?.scrollIntoView({ block: 'nearest' });
  }
  // Guard against double-submission from rapid key presses.
  let isSubmitting = false;

  async function setAutonomy(val: AutonomyLevel) {
    const previous = autonomyLevel;
    autonomyLevel = val; // optimistic
    if (!settings) {
      // Settings not loaded yet — keep the optimistic UI but don't claim it persisted.
      return;
    }
    try {
      const next = { ...settings, juryAutonomyLevel: val };
      await api.updateSettings(next);
      settings = next;
      // The GLOBAL store too (2026-08-20 hunt): SettingsPanel seeds its inputs from it and persists
      // the WHOLE object, so a store left stale here meant "change the theme, close Settings" wrote
      // the old autonomy level back over the owner's explicit dial choice — silently.
      settingsStore.set({ ...next });
      statusMsg = $t('inbox.status.autonomySet', {
        level: $t(autonomyLabelKey(val)),
      });
    } catch (e) {
      autonomyLevel = previous; // revert so the dial never lies about the persisted state
      statusMsg = $t('inbox.status.autonomyFailed', { err: publicErrorMessage(e) });
    }
  }

  $: current = queue[currentIndex] ?? null;
  $: currentEligibility = current ? queueEligibility[current.id] : null;
  $: currentRevision = current ? queueRevisions[current.id] : undefined;
  $: currentSelectionKey = current
    ? `${current.id}\0${typeof currentRevision === 'number' ? currentRevision : 'missing'}`
    : null;
  $: ensureActiveSelection(currentSelectionKey, current, currentRevision);

  function ensureActiveSelection(
    selectionKey: string | null,
    selected: SpeechSegment | null,
    revision: number | undefined,
  ) {
    if (selectionKey === activeSelectionKey) return;
    activeSelectionKey = selectionKey;
    void activateSelection(selected, revision);
  }
  let draftAuthorityBlockedKey: TranslationKey | null;
  let sharedDecisionDisabledKey: TranslationKey | null;
  let acceptDisabledKey: TranslationKey | null;
  let editDisabledKey: TranslationKey | null;
  let rejectDisabledKey: TranslationKey | null;
  let skipDisabledKey: TranslationKey | null;
  let flagDisabledKey: TranslationKey | null;
  let saveEditDisabledKey: TranslationKey | null;
  let undoDisabledKey: TranslationKey | null;

  $: draftAuthorityBlockedKey = !current
    ? null
    : draftLoadError
      ? 'inbox.disabled.draftUnavailable'
      : draftReadyId !== current.id
        ? 'inbox.disabled.draftLoading'
        : draftConflict
          ? 'inbox.disabled.draftConflict'
          : draftPending && !isEditing
            ? 'inbox.disabled.draftPending'
            : null;
  $: authorityUnavailable =
    !!current &&
    (!currentEligibility?.eligible ||
      typeof currentRevision !== 'number' ||
      !Number.isSafeInteger(currentRevision) ||
      currentRevision < 0);
  $: pendingCount = queue.filter((s) => !s.humanDecision).length;
  $: activeQueueAnnouncement =
    announcedQueueIndex == null || !queue[announcedQueueIndex]
      ? ''
      : $t('inbox.activeItem', {
          position: String(announcedQueueIndex + 1),
          total: String(queue.length),
        });
  $: sharedDecisionDisabledKey = isSubmitting
    ? 'inbox.disabled.saving'
    : draftAuthorityBlockedKey
      ? draftAuthorityBlockedKey
      : current?.humanDecision
        ? 'inbox.disabled.alreadyReviewed'
        : authorityUnavailable
          ? 'inbox.disabled.notEligible'
          : audioError
            ? 'inbox.disabled.audioUnavailable'
            : null;
  $: acceptDisabledKey = isEditing ? 'review.acceptDisabledEdited' : sharedDecisionDisabledKey;
  $: editDisabledKey = isEditing
    ? 'inbox.disabled.editInProgress'
    : isSubmitting
      ? 'inbox.disabled.saving'
      : draftLoadError
        ? 'inbox.disabled.draftUnavailable'
        : draftReadyId !== current?.id
          ? 'inbox.disabled.draftLoading'
          : draftConflict
            ? 'inbox.disabled.draftConflict'
            : current?.humanDecision
              ? 'inbox.disabled.alreadyReviewed'
              : authorityUnavailable
                ? 'inbox.disabled.notEligible'
                : null;
  $: rejectDisabledKey = isEditing ? 'inbox.disabled.editInProgress' : sharedDecisionDisabledKey;
  $: skipDisabledKey = isSubmitting ? 'inbox.disabled.saving' : null;
  $: flagDisabledKey = isEditing
    ? 'inbox.disabled.editInProgress'
    : isSubmitting
      ? 'inbox.disabled.saving'
      : draftAuthorityBlockedKey
        ? draftAuthorityBlockedKey
        : current?.humanDecision
          ? 'inbox.disabled.alreadyReviewed'
          : null;
  $: saveEditDisabledKey = sharedDecisionDisabledKey
    ? sharedDecisionDisabledKey
    : !editText.trim()
      ? 'inbox.disabled.emptyEdit'
      : editingForId !== current?.id
        ? 'inbox.disabled.staleEdit'
        : null;
  $: undoDisabledKey = isEditing
    ? 'inbox.disabled.editInProgress'
    : isSubmitting
      ? 'inbox.disabled.saving'
      : draftAuthorityBlockedKey
        ? draftAuthorityBlockedKey
        : history.length === 0
          ? 'inbox.disabled.noUndo'
          : null;
  // Bound the player to this segment's window so Play hears only this clip, not the whole file.
  $: inboxRange = current
    ? chunkPlaybackRange(parseSourceMeta(current.alignmentJson))
    : { startTime: 0, endTime: 0 };

  // ── Confidence bands ─────────────────────────────────────────────────────────
  // `tr` ($t) is passed in from the template so the band labels stay reactive to a locale change.
  /// Poor audio, by the same thresholds `has_hard_distrust_veto` uses in the jury (snr < 5 dB or
  /// clipping > 0.1). Kept identical on purpose: two definitions of "bad audio" that drift apart would
  /// show a green chip on a clip the gate refused to trust.
  function hasPoorAudio(seg: { snrDb?: number | null; clippingRatio?: number | null }): boolean {
    return (
      (seg.snrDb != null && seg.snrDb < 5) || (seg.clippingRatio != null && seg.clippingRatio > 0.1)
    );
  }

  // ── Queue loading ─────────────────────────────────────────────────────────────
  function isActiveDraftTarget(segmentId: string, baseRevision: number): boolean {
    return current?.id === segmentId && currentRevision === baseRevision;
  }

  function refreshDraftSaving() {
    draftSaving = !!current && draftWrites.isWriting(current.id);
  }

  function queueDraftWrite(segmentId: string, baseRevision: number, text: string): Promise<void> {
    return draftWrites.request({ kind: 'save', segmentId, baseRevision, text });
  }

  function clearDraftSaveTimer() {
    if (draftSaveTimer !== null) {
      clearTimeout(draftSaveTimer);
      draftSaveTimer = null;
    }
  }

  function scheduleDraftSave() {
    clearDraftSaveTimer();
    const segmentId = editingForId;
    const baseRevision = currentRevision;
    if (
      !isEditing ||
      !segmentId ||
      current?.id !== segmentId ||
      typeof baseRevision !== 'number' ||
      draftReadyId !== segmentId
    ) {
      return;
    }
    const text = editText;
    draftSaveTimer = setTimeout(() => {
      draftSaveTimer = null;
      void queueDraftWrite(segmentId, baseRevision, text).catch(() => undefined);
    }, 500);
  }

  function handleEditInput(text: string) {
    editText = text;
    draftPending = editText.trim() !== draftBaseline.trim();
    scheduleDraftSave();
  }

  async function loadReviewDraft(seg: SpeechSegment, baseRevision: number, generation: number) {
    const sequence = ++draftLoadSequence;
    try {
      await draftWrites.flushSegment(seg.id);
      const draft = await api.getReviewDraftV1(seg.id);
      if (
        sequence !== draftLoadSequence ||
        generation !== selectionGeneration ||
        !isActiveDraftTarget(seg.id, baseRevision)
      ) {
        return;
      }
      draftConflict = null;
      draftRecovered = false;
      draftSaveFailed = false;
      draftLoadError = null;
      if (!draft) {
        draftWrites.acknowledge({ kind: 'delete', segmentId: seg.id, baseRevision });
      } else if (draft.segmentId !== seg.id) {
        throw new Error($t('inbox.error.draftIdentityMismatch'));
      } else if (draft.baseRevision === baseRevision) {
        draftWrites.acknowledge({
          kind: 'save',
          segmentId: seg.id,
          baseRevision,
          text: draft.text,
        });
        editText = draft.text;
        draftPending = draft.text.trim() !== draftBaseline.trim();
        draftRecovered = draftPending;
        isEditing = draftPending;
        editingForId = draftPending ? seg.id : null;
        if (isEditing) void tick().then(() => editTextarea?.focus());
      } else {
        draftConflict = draft;
      }
      draftReadyId = seg.id;
    } catch (error) {
      if (
        sequence !== draftLoadSequence ||
        generation !== selectionGeneration ||
        !isActiveDraftTarget(seg.id, baseRevision)
      ) {
        return;
      }
      draftLoadError = api.reviewErrorMessage(error, $t('review.draftLoadFailedHint'));
      statusMsg = $t('review.draftLoadFailed');
      draftReadyId = null;
    }
  }

  async function activateSelection(seg: SpeechSegment | null, revision: number | undefined) {
    clearDraftSaveTimer();
    const generation = ++selectionGeneration;
    inboxPlaying = false;
    inboxCurrentTime = 0;
    inboxPlaybackReceiptId = null;
    inboxPlaybackMediaGrantId = null;
    inboxPlaybackClipDurationMs = null;
    inboxHeardIntervals = [];
    audioError = null;
    technicalUnusableReason = '';
    technicalUnusableIntent = null;
    isEditing = false;
    editText = '';
    editingForId = null;
    draftBaseline = '';
    draftReadyId = null;
    draftConflict = null;
    draftRecovered = false;
    draftSaveFailed = false;
    draftLoadError = null;
    draftPending = false;
    refreshDraftSaving();
    if (!seg || typeof revision !== 'number') return;
    draftBaseline = seg.verdictTranscript ?? seg.rawTranscript ?? '';
    editText = draftBaseline;
    await loadReviewDraft(seg, revision, generation);
  }

  async function retryDraftLoad() {
    const seg = current;
    const revision = currentRevision;
    if (!seg || typeof revision !== 'number') return;
    draftLoadError = null;
    await loadReviewDraft(seg, revision, selectionGeneration);
  }

  function useConflictingDraft() {
    const conflict = draftConflict;
    if (!current || !conflict || typeof currentRevision !== 'number') return;
    editText = conflict.text;
    editingForId = current.id;
    isEditing = true;
    draftPending = editText.trim() !== draftBaseline.trim();
    draftRecovered = true;
    draftConflict = null;
    scheduleDraftSave();
    void tick().then(() => editTextarea?.focus());
  }

  async function flushActiveReviewDraft(): Promise<void> {
    clearDraftSaveTimer();
    const segmentId = editingForId;
    const revision = currentRevision;
    if (
      isEditing &&
      segmentId &&
      current?.id === segmentId &&
      typeof revision === 'number' &&
      draftReadyId === segmentId &&
      draftPending
    ) {
      await queueDraftWrite(segmentId, revision, editText);
    }
    await draftWrites.flushAll();
  }

  async function cancelEdit() {
    try {
      await flushActiveReviewDraft();
      isEditing = false;
      if (draftPending) statusMsg = $t('inbox.status.draftKept');
    } catch {
      statusMsg = $t('review.draftSaveFailedHint');
      void tick().then(() => editTextarea?.focus());
    }
  }

  let closePending = false;
  async function requestClose() {
    if (closePending) return;
    closePending = true;
    try {
      await flushActiveReviewDraft();
      onClose();
    } catch {
      statusMsg = $t('review.closeDraftFailed');
      void tick().then(() => editTextarea?.focus());
    } finally {
      closePending = false;
    }
  }

  async function loadQueue() {
    try {
      await flushActiveReviewDraft();
    } catch {
      statusMsg = $t('review.closeDraftFailed');
      return;
    }
    const generation = ++queueLoadGeneration;
    isLoading = true;
    isLoadingMore = false;
    loadError = null;
    loadMoreError = null;
    try {
      const page = await api.getReviewPageV1({ kind: 'escalation' }, null, INBOX_PAGE_SIZE);
      if (generation !== queueLoadGeneration) return;
      queue = page.items.map((item) => item.segment);
      queueRevisions = Object.fromEntries(
        page.items.map((item) => [item.segment.id, item.baseRevision]),
      );
      queueEligibility = Object.fromEntries(
        page.items.map((item) => [
          item.segment.id,
          { eligible: item.eligible, disabledReason: item.disabledReason },
        ]),
      );
      queueTotal = page.total;
      nextCursor = page.nextCursor;
      evictedCount = 0;
      currentIndex = 0;
      inboxPlaybackReceiptId = null;
      inboxPlaybackMediaGrantId = null;
      inboxPlaybackClipDurationMs = null;
      inboxHeardIntervals = [];
      audioError = null;
      technicalUnusableReason = '';
      technicalUnusableIntent = null;
      announcedQueueIndex = null;
      // Drop the undo stack: it references the PREVIOUS queue's segments. A stale undo after a
      // reload would fire a backend clear against a segment no longer in view.
      history = [];
    } catch (e) {
      if (generation !== queueLoadGeneration) return;
      loadError = $t('inbox.status.loadFailed', { err: publicErrorMessage(e) });
      statusMsg = loadError;
    } finally {
      if (generation === queueLoadGeneration) isLoading = false;
    }
  }

  function mergeReviewPage(page: ReviewPageV1) {
    const selectedId = current?.id ?? null;
    const combined = [...queue];
    const indexById = new Map(combined.map((row, index) => [row.id, index]));
    const revisions = { ...queueRevisions };
    const eligibility = { ...queueEligibility };
    for (const item of page.items) {
      const existingIndex = indexById.get(item.segment.id);
      // The suspect-first keyset can legitimately return an earlier row again when concurrent quality
      // metadata changes its sort position between pages. Never replace the selected snapshot/revision:
      // doing so re-runs activateSelection, clears the active editor and can lose the last sub-500ms
      // correction before its draft timer fires. The eventual typed commit remains CAS-protected and a
      // stale revision reloads server truth; unselected duplicates can safely take the fresher snapshot.
      if (existingIndex !== undefined && item.segment.id === selectedId) continue;
      revisions[item.segment.id] = item.baseRevision;
      eligibility[item.segment.id] = {
        eligible: item.eligible,
        disabledReason: item.disabledReason,
      };
      if (existingIndex === undefined) {
        indexById.set(item.segment.id, combined.length);
        combined.push(item.segment);
      } else {
        combined[existingIndex] = item.segment;
      }
    }

    let retained = combined;
    if (combined.length > MAX_RESIDENT_ROWS) {
      const retainedIds = new Set(
        combined.slice(combined.length - MAX_RESIDENT_ROWS).map((row) => row.id),
      );
      if (selectedId && !retainedIds.has(selectedId)) {
        const oldestNewerId = combined.find((row) => retainedIds.has(row.id))?.id;
        if (oldestNewerId) retainedIds.delete(oldestNewerId);
        retainedIds.add(selectedId);
      }
      retained = combined.filter((row) => retainedIds.has(row.id));
      evictedCount += combined.length - retained.length;
    }

    const retainedIds = new Set(retained.map((row) => row.id));
    queueRevisions = Object.fromEntries(
      Object.entries(revisions).filter(([segmentId]) => retainedIds.has(segmentId)),
    );
    queueEligibility = Object.fromEntries(
      Object.entries(eligibility).filter(([segmentId]) => retainedIds.has(segmentId)),
    );
    queue = retained;
    const selectedIndex = selectedId ? queue.findIndex((row) => row.id === selectedId) : -1;
    currentIndex = selectedIndex >= 0 ? selectedIndex : Math.min(currentIndex, queue.length - 1);
    if (currentIndex < 0) currentIndex = 0;
    if (announcedQueueIndex !== null) announcedQueueIndex = currentIndex;
  }

  async function loadMoreQueue() {
    const cursor = nextCursor;
    if (!cursor || isLoadingMore) return;
    const generation = queueLoadGeneration;
    isLoadingMore = true;
    loadMoreError = null;
    try {
      const page = await api.getReviewPageV1({ kind: 'escalation' }, cursor, INBOX_PAGE_SIZE);
      if (generation !== queueLoadGeneration || cursor !== nextCursor) return;
      mergeReviewPage(page);
      queueTotal = page.total;
      nextCursor = page.nextCursor === cursor ? null : page.nextCursor;
    } catch (error) {
      if (generation !== queueLoadGeneration || cursor !== nextCursor) return;
      loadMoreError = $t('inbox.status.loadMoreFailed', { err: publicErrorMessage(error) });
      statusMsg = loadMoreError;
    } finally {
      if (generation === queueLoadGeneration) isLoadingMore = false;
    }
  }

  function maybeLoadMore(index: number) {
    if (nextCursor && index >= queue.length - NEAR_END_THRESHOLD) void loadMoreQueue();
  }

  let isRunningJury = false;

  async function triggerJuryPipeline() {
    if (isRunningJury) return;
    isRunningJury = true;
    statusMsg = $t('inbox.status.running');
    try {
      await flushActiveReviewDraft();
      const targetIds = await api.getSegmentIdsForView({ verified: false });
      if (targetIds.length === 0) {
        statusMsg = $t('inbox.status.noUnverified');
        isRunningJury = false;
        return;
      }
      const report = await api.runJuryPipeline(targetIds);
      if (!report) throw new Error($t('inbox.error.juryNoResult'));
      statusMsg = $t('inbox.status.juryFinished', {
        t0: String(report.t0AutoAccepted ?? 0),
        t1: String(report.t1Committed ?? 0),
        t2: String(report.t2Committed ?? 0),
        esc: String(report.humanInbox ?? 0),
      });
      await loadQueue();
    } catch (e) {
      statusMsg = $t('inbox.status.juryFailed', { err: publicErrorMessage(e) });
    } finally {
      isRunningJury = false;
    }
  }

  // ── Actions ──────────────────────────────────────────────────────────────────
  type ReviewDecision = 'accept' | 'edit' | 'reject';

  function publicErrorMessage(error: unknown): string {
    return formatPublicErrorReference(error) ?? $t('inbox.error.unknown');
  }

  function isPlaybackEvidenceError(error: unknown): boolean {
    if (!error || typeof error !== 'object') {
      return error instanceof Error && error.message.includes('E_NO_PLAYBACK_EVIDENCE');
    }
    const code = (error as { code?: unknown }).code;
    return (
      code === 'PLAYBACK_RECEIPT_REQUIRED' ||
      code === 'INVALID_PLAYBACK_RECEIPT' ||
      code === 'NO_PLAYBACK_EVIDENCE' ||
      code === 'E_NO_PLAYBACK_EVIDENCE' ||
      (typeof (error as { message?: unknown }).message === 'string' &&
        (error as { message: string }).message.includes('E_NO_PLAYBACK_EVIDENCE'))
    );
  }

  function decisionFailure(key: TranslationKey, error: unknown): string {
    return isPlaybackEvidenceError(error)
      ? $t('review.mustListen')
      : $t(key, { err: publicErrorMessage(error) });
  }

  function requireBaseRevision(seg: SpeechSegment): number | null {
    const revision = queueRevisions[seg.id];
    const eligibility = queueEligibility[seg.id];
    if (
      !eligibility?.eligible ||
      typeof revision !== 'number' ||
      !Number.isSafeInteger(revision) ||
      revision < 0
    ) {
      statusMsg = $t('inbox.disabled.notEligible');
      return null;
    }
    return revision;
  }

  async function finalizePlaybackReceipt(
    seg: SpeechSegment,
    baseRevision: number,
  ): Promise<string | null> {
    // Pause and snapshot in the child that owns the media clock. A parent flag assignment cannot
    // synchronously accrue the final between-timeupdate delta, and navigation may replace this keyed
    // instance while IPC is in flight.
    const authority = await inboxAudioPlayer?.pauseAndSnapshot();
    inboxPlaying = false;
    if (
      !authority ||
      authority.segmentId !== seg.id ||
      authority.segmentRevision !== baseRevision
    ) {
      statusMsg = $t('review.mustListen');
      return null;
    }
    const alreadyFinalized = playbackAttempts.finalizedReceipt(seg.id, baseRevision);
    if (alreadyFinalized) return alreadyFinalized;
    const playbackReceiptId = authority.playbackReceiptId;
    const mediaGrantId = authority.mediaGrantId;
    const clipDurationMs = authority.clipDurationMs;
    const intervals = authority.intervals.map(({ startMs, endMs }) => ({ startMs, endMs }));
    if (
      !playbackReceiptId ||
      !mediaGrantId ||
      !Number.isSafeInteger(clipDurationMs) ||
      (clipDurationMs ?? 0) <= 0 ||
      !hasSufficientReviewPlayback(intervals, clipDurationMs as number)
    ) {
      statusMsg = $t('review.mustListen');
      return null;
    }
    const attempt = playbackAttempts.snapshot({
      segmentId: seg.id,
      baseRevision,
      playbackReceiptId,
      mediaGrantId,
      intervals,
    });
    try {
      const finalized = await api.recordPlaybackReceipt({
        playbackReceiptId: attempt.playbackReceiptId,
        mediaGrantId: attempt.mediaGrantId,
        intervals: attempt.intervals,
      });
      if (
        finalized.playbackReceiptId !== attempt.playbackReceiptId ||
        finalized.segmentId !== seg.id ||
        finalized.segmentRevision !== baseRevision
      ) {
        throw new Error($t('inbox.error.playbackReceiptMismatch'));
      }
      playbackAttempts.markFinalized(seg.id, baseRevision, finalized.playbackReceiptId);
      return finalized.playbackReceiptId;
    } catch (error) {
      // Only explicit pre-commit server outcomes may retire this receipt. Transport/unknown failures
      // remain frozen because finalization can be durable before the response reaches the renderer.
      if (isProvenUncommittedPlaybackFinalization(error)) {
        playbackAttempts.resolve(seg.id, baseRevision);
        inboxAudioPlayer?.restartPlaybackAuthority();
      }
      throw error;
    }
  }

  function committedRow(
    seg: SpeechSegment,
    decision: ReviewDecision,
    authoritativeTranscript: string,
  ): SpeechSegment {
    return {
      ...seg,
      verified: true,
      verdict: `human_${decision}`,
      humanDecision: decision,
      verdictTranscript: authoritativeTranscript,
      annotatedTranscript: decision === 'edit' ? authoritativeTranscript : seg.annotatedTranscript,
    };
  }

  function publishTypedCommit(
    seg: SpeechSegment,
    decision: ReviewDecision,
    baseRevision: number,
    commit: {
      segmentId: string;
      committedRevision: number;
      authoritativeTranscript: string;
      decisionId: string;
    },
  ): number | null {
    if (!isCommittedReviewFor(commit, seg.id, baseRevision)) {
      statusMsg = $t('inbox.status.loadFailed', {
        err: $t('inbox.error.commitIdentityMismatch'),
      });
      void loadQueue();
      return null;
    }
    const effectEventId = api.reviewEffectId(commit.decisionId);
    if (effectEventId === null) {
      // The write is already durable. Mark it locally before reloading so the renderer cannot offer a
      // duplicate decision while recovering from a malformed response, but never invent undo authority.
      applyCommittedRow(committedRow(seg, decision, commit.authoritativeTranscript));
      statusMsg = $t('inbox.status.loadFailed', {
        err: $t('inbox.error.commitIdentityMismatch'),
      });
      void loadQueue();
      return null;
    }
    queueRevisions = { ...queueRevisions, [seg.id]: commit.committedRevision };
    applyCommittedRow(committedRow(seg, decision, commit.authoritativeTranscript));
    return effectEventId;
  }

  async function accept() {
    if (audioError) return; // unplayable audio: refuse the verdict (see audioError)
    if (isEditing) return; // never discard the correction in progress and certify the old text

    // Already-decided guard: advance() does NOT move past the LAST queue item, so without this a
    // second keypress on the final clip would record a DUPLICATE human decision (a biometric label).
    if (!current || isSubmitting || current.humanDecision) return;
    // Snapshot the target before the await — currentIndex/current can change mid-flight if the user
    // clicks another rail item (the rail is not disabled during submit), which would otherwise stamp
    // this decision onto the wrong segment. applyCommittedRow puts the committed row back BY ID.
    const cur = current;
    const baseRevision = requireBaseRevision(cur);
    if (baseRevision === null) return;
    isSubmitting = true;
    let commitIntent: ReviewCommitIntent;
    try {
      const playbackReceiptId = await finalizePlaybackReceipt(cur, baseRevision);
      if (!playbackReceiptId) return;
      // Accept exactly the transcript visible in this focus card. Passing null could preserve a
      // different, unseen backend candidate as human-approved gold.
      const transcript = cur.verdictTranscript ?? cur.rawTranscript ?? '';
      commitIntent = {
        segmentId: cur.id,
        baseRevision,
        decision: 'accept',
        transcript,
        reasonCode: null,
        playbackReceiptId,
      };
      const commit = await api.commitReviewV1({
        operationId: commitOperations.idFor(commitIntent),
        ...commitIntent,
      });
      const effectEventId = publishTypedCommit(cur, 'accept', baseRevision, commit);
      if (effectEventId === null) return;
      commitOperations.resolve(commitIntent);
      playbackAttempts.resolve(cur.id, baseRevision);
      history = [
        ...history,
        {
          kind: 'human',
          id: cur.id,
          decision: 'accept',
          effectEventId,
          operationId: crypto.randomUUID(),
        },
      ];
      const visibleId = current?.id ?? null;
      statusMsg = $t('inbox.status.accepted');
      if (visibleId === cur.id) void advance();
    } catch (e) {
      statusMsg = decisionFailure('inbox.status.acceptFailed', e);
      if (api.isCommandErrorV1(e, 'STALE_REVISION')) void loadQueue();
    } finally {
      isSubmitting = false;
    }
  }

  async function startEdit() {
    if (
      !current ||
      isEditing ||
      isSubmitting ||
      current.humanDecision ||
      draftReadyId !== current.id ||
      draftLoadError ||
      draftConflict
    )
      return;
    if (editingForId !== current.id) editText = draftBaseline;
    isEditing = true;
    editingForId = current.id;
    await tick();
    editTextarea?.focus();
    editTextarea?.select();
  }

  async function commitEdit() {
    if (audioError) return; // unplayable audio: refuse the verdict, same as accept/reject
    if (!current || !editText.trim() || isSubmitting || current.humanDecision) return;
    // Never write text opened for one segment onto another. Keep the text in its crash-safe draft;
    // silently clearing it here would turn a navigation race into human-work loss.
    if (editingForId !== current.id) {
      statusMsg = $t('inbox.disabled.staleEdit');
      return;
    }
    const cur = current;
    const baseRevision = requireBaseRevision(cur);
    if (baseRevision === null) return;
    const text = editText.trim();
    isSubmitting = true;
    let commitIntent: ReviewCommitIntent;
    try {
      // Navigation, native close and submission share this same durable barrier. If the draft write
      // fails, playback and authoritative commit do not begin and the clip/editor remain unchanged.
      await flushActiveReviewDraft();
      if (current?.id !== cur.id || currentRevision !== baseRevision || editText.trim() !== text) {
        statusMsg = $t('inbox.status.draftChangedDuringSave');
        return;
      }
      const playbackReceiptId = await finalizePlaybackReceipt(cur, baseRevision);
      if (!playbackReceiptId) return;
      commitIntent = {
        segmentId: cur.id,
        baseRevision,
        decision: 'edit',
        transcript: text,
        reasonCode: null,
        playbackReceiptId,
      };
      const commit = await api.commitReviewV1({
        operationId: commitOperations.idFor(commitIntent),
        ...commitIntent,
      });
      const effectEventId = publishTypedCommit(cur, 'edit', baseRevision, commit);
      if (effectEventId === null) return;
      commitOperations.resolve(commitIntent);
      playbackAttempts.resolve(cur.id, baseRevision);
      history = [
        ...history,
        {
          kind: 'human',
          id: cur.id,
          decision: 'edit',
          effectEventId,
          operationId: crypto.randomUUID(),
        },
      ];
      const visibleId = current?.id ?? null;
      draftWrites.acknowledge({ kind: 'delete', segmentId: cur.id, baseRevision });
      if (visibleId === cur.id) {
        isEditing = false;
        draftPending = false;
        draftRecovered = false;
        editingForId = null;
      }
      statusMsg = $t('inbox.status.edited');
      if (visibleId === cur.id) void advance();
    } catch (e) {
      statusMsg = decisionFailure('inbox.status.editFailed', e);
      if (api.isCommandErrorV1(e, 'STALE_REVISION')) void loadQueue();
    } finally {
      isSubmitting = false;
    }
  }

  async function reject() {
    if (audioError) return; // unplayable audio: refuse the verdict (see audioError)
    if (isEditing) return; // a correction in progress remains authoritative until save/cancel

    if (!current || isSubmitting || current.humanDecision) return;
    const cur = current;
    const baseRevision = requireBaseRevision(cur);
    if (baseRevision === null) return;
    isSubmitting = true;
    let commitIntent: ReviewCommitIntent;
    try {
      const playbackReceiptId = await finalizePlaybackReceipt(cur, baseRevision);
      if (!playbackReceiptId) return;
      commitIntent = {
        segmentId: cur.id,
        baseRevision,
        decision: 'reject',
        transcript: null,
        reasonCode: null,
        playbackReceiptId,
      };
      const commit = await api.commitReviewV1({
        operationId: commitOperations.idFor(commitIntent),
        ...commitIntent,
      });
      const effectEventId = publishTypedCommit(cur, 'reject', baseRevision, commit);
      if (effectEventId === null) return;
      commitOperations.resolve(commitIntent);
      playbackAttempts.resolve(cur.id, baseRevision);
      history = [
        ...history,
        {
          kind: 'human',
          id: cur.id,
          decision: 'reject',
          effectEventId,
          operationId: crypto.randomUUID(),
        },
      ];
      const visibleId = current?.id ?? null;
      statusMsg = $t('inbox.status.rejected');
      if (visibleId === cur.id) void advance();
    } catch (e) {
      statusMsg = decisionFailure('inbox.status.rejectFailed', e);
      if (api.isCommandErrorV1(e, 'STALE_REVISION')) void loadQueue();
    } finally {
      isSubmitting = false;
    }
  }

  function isMarkedUnusableResponse(
    response: api.MarkedSegmentUnusableV1,
    request: api.MarkSegmentUnusableRequestV1,
  ): boolean {
    return (
      response.segmentId === request.segmentId &&
      response.committedRevision === request.baseRevision + 1 &&
      response.reason === request.reason &&
      /^flag-effect:[1-9][0-9]*$/.test(response.effectId)
    );
  }

  /** A technical media disposition is not accept/edit/reject and therefore has no playback gate. */
  async function markTechnicallyUnusable() {
    const cur = current;
    const reason = technicalUnusableReason;
    if (!cur || !audioError || !reason || isSubmitting || isEditing || !!draftAuthorityBlockedKey)
      return;
    const baseRevision = queueRevisions[cur.id];
    if (
      typeof baseRevision !== 'number' ||
      !Number.isSafeInteger(baseRevision) ||
      baseRevision < 0
    ) {
      statusMsg = $t('review.unusable.authorityMissing');
      return;
    }
    const matchingIntent =
      technicalUnusableIntent?.segmentId === cur.id &&
      technicalUnusableIntent.baseRevision === baseRevision &&
      technicalUnusableIntent.reason === reason
        ? technicalUnusableIntent
        : null;
    const request: api.MarkSegmentUnusableRequestV1 = matchingIntent ?? {
      operationId: crypto.randomUUID(),
      segmentId: cur.id,
      baseRevision,
      reason,
    };
    technicalUnusableIntent = request;
    isSubmitting = true;
    try {
      const response = await api.markSegmentUnusableV1(request);
      if (!isMarkedUnusableResponse(response, request)) {
        statusMsg = $t('review.unusable.invalidResponse');
        return;
      }
      technicalUnusableIntent = null;
      const visibleId = current?.id ?? null;
      queue = queue.filter((row) => row.id !== cur.id);
      const remainingRevisions = { ...queueRevisions };
      delete remainingRevisions[cur.id];
      queueRevisions = remainingRevisions;
      const remainingEligibility = { ...queueEligibility };
      delete remainingEligibility[cur.id];
      queueEligibility = remainingEligibility;
      inboxPlaying = false;
      inboxCurrentTime = 0;
      inboxPlaybackReceiptId = null;
      inboxPlaybackMediaGrantId = null;
      inboxPlaybackClipDurationMs = null;
      inboxHeardIntervals = [];
      audioError = null;
      technicalUnusableReason = '';
      if (queue.length > 0) {
        if (visibleId === cur.id) currentIndex = Math.min(currentIndex, queue.length - 1);
        else {
          const visibleIndex = visibleId ? queue.findIndex((row) => row.id === visibleId) : -1;
          currentIndex =
            visibleIndex >= 0 ? visibleIndex : Math.min(currentIndex, queue.length - 1);
        }
        announcedQueueIndex = currentIndex;
      } else {
        currentIndex = 0;
        announcedQueueIndex = null;
      }
      statusMsg = $t('review.unusable.marked');
    } catch (error) {
      statusMsg = $t('review.unusable.markFailedWithError', {
        err: api.reviewErrorMessage(error, $t('review.unusable.markFailedHint')),
      });
    } finally {
      isSubmitting = false;
    }
  }

  async function skip() {
    if (!current || isSubmitting) return;
    statusMsg = $t('inbox.status.skipped');
    await advance();
  }

  // Guard against a malformed evidence_json (truncated / legacy / externally-written row): a raw
  // JSON.parse in the template throws synchronously during render and breaks the focus card so the
  // segment can't be adjudicated. Fall back to showing the raw string.
  function safeEvidence(j: string | null | undefined): string {
    try {
      return JSON.stringify(JSON.parse(j ?? '[]'), null, 2);
    } catch {
      return j ?? '';
    }
  }

  async function flag() {
    if (
      !current ||
      isEditing ||
      isSubmitting ||
      current.humanDecision ||
      !!draftAuthorityBlockedKey
    )
      return;
    const cur = current;
    isSubmitting = true;
    try {
      const commit = await api.recordReviewFlag(cur.id, 'Flagged for second-pass adjudication');
      history = [
        ...history,
        {
          kind: 'flag',
          id: cur.id,
          decision: 'flag',
          effectEventId: commit.effectEventId,
          operationId: crypto.randomUUID(),
        },
      ];
      queueRevisions = { ...queueRevisions, [cur.id]: commit.flagRevision };
      applyCommittedRow(commit.segment);
      statusMsg = $t('inbox.status.flagged');
      void advance();
    } catch (e) {
      statusMsg = $t('inbox.status.flagFailed', { err: publicErrorMessage(e) });
    } finally {
      isSubmitting = false;
    }
  }

  async function undo() {
    // Guard against racing any in-flight write. Decisions and flags publish history only after their
    // atomic commit; Undo must never overlap a mutation of the same queue/history state.
    if (isSubmitting) return;
    if (isEditing) return;
    const last = history[history.length - 1];
    if (!last) return;
    isSubmitting = true;
    history = history.slice(0, -1); // reassignment: keeps the Undo button's disabled binding live
    try {
      // Flag is not a human decision and keeps its dedicated inverse. A human decision is reversed by
      // immutable effect id; no renderer snapshot or blanket "clear" is trusted.
      let restored: SpeechSegment;
      let restoredRevision: number | null = null;
      if (last.kind === 'flag') {
        const outcome = await api.undoReviewFlag(last.effectEventId, last.operationId);
        if (outcome.status === 'conflict') {
          history = [...history, last];
          statusMsg = $t('inbox.status.undoFailed', {
            err: $t('inbox.error.undoFlagConflict'),
          });
          return;
        }
        restored = outcome.segment;
        restoredRevision = outcome.status === 'applied' ? outcome.restoredRevision : null;
      } else {
        const outcome = await api.undoHumanDecision(last.effectEventId, last.operationId);
        if (outcome.status === 'conflict') {
          history = [...history, last];
          statusMsg = $t('inbox.status.undoFailed', {
            err: $t('inbox.error.undoDecisionConflict'),
          });
          return;
        }
        restored = outcome.segment;
        restoredRevision = outcome.restoredRevision;
      }
      const idx = queue.findIndex((s) => s.id === last.id);
      if (idx >= 0) {
        queue[idx] = restored;
        if (restoredRevision !== null) {
          queueRevisions = { ...queueRevisions, [last.id]: restoredRevision };
        }
        // Navigate directly to the undone segment, not blindly to currentIndex-1.
        // If the user accepted seg#5 then scrolled to seg#10, undo should show seg#5.
        await selectQueueIndex(idx, true, false);
      }
      statusMsg = $t('inbox.status.undone');
      // The legacy flag inverse omits the restored revision on an idempotent replay. Reload the typed
      // page rather than guessing; a later policy-4 decision must never use the pre-flag revision.
      if (last.kind === 'flag' && restoredRevision === null) void loadQueue();
    } catch (e) {
      // The decision was NOT cleared — put the history entry back so the undo can be
      // retried, and tell the user instead of failing silently (which previously also
      // dropped the entry, making the undo permanently unretryable).
      history = [...history, last];
      statusMsg = $t('inbox.status.undoFailed', { err: publicErrorMessage(e) });
    } finally {
      isSubmitting = false;
    }
  }

  async function advance() {
    if (currentIndex < queue.length - 1) {
      await selectQueueIndex(currentIndex + 1, true, false);
      return;
    }
    const activeId = current?.id ?? null;
    if (nextCursor) {
      await loadMoreQueue();
      const activeIndex = activeId ? queue.findIndex((row) => row.id === activeId) : -1;
      if (activeIndex >= 0 && activeIndex < queue.length - 1) {
        await selectQueueIndex(activeIndex + 1, true, false);
      }
    }
  }

  function queueOptionId(index: number): string {
    return `review-inbox-option-${index}`;
  }

  function queueOptionLabel(seg: SpeechSegment, index: number): string {
    return $t(seg.humanDecision ? 'inbox.queueItemReviewed' : 'inbox.queueItem', {
      position: String(index + 1),
      total: String(queue.length),
      id: seg.id,
    });
  }

  async function focusQueueListbox() {
    await tick();
    queueListbox?.focus({ preventScroll: true });
  }

  let navigationSequence = 0;
  async function selectQueueIndex(index: number, announce: boolean, focusListbox: boolean) {
    if (queue.length === 0) return false;
    const next = Math.max(0, Math.min(index, queue.length - 1));
    if (next === currentIndex) {
      if (focusListbox) void focusQueueListbox();
      maybeLoadMore(next);
      return true;
    }
    const sequence = ++navigationSequence;
    try {
      await flushActiveReviewDraft();
    } catch {
      statusMsg = $t('review.closeDraftFailed');
      void tick().then(() => editTextarea?.focus());
      return false;
    }
    if (sequence !== navigationSequence) return false;
    currentIndex = next;
    if (announce) announcedQueueIndex = next;
    if (focusListbox) void focusQueueListbox();
    maybeLoadMore(next);
    return true;
  }

  function handleQueueListboxKey(e: KeyboardEvent) {
    const key = physicalKey(e);
    let next: number | null = null;
    if (key === 'ArrowDown' || key === 'ArrowRight') next = currentIndex + 1;
    else if (key === 'ArrowUp' || key === 'ArrowLeft') next = currentIndex - 1;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = queue.length - 1;
    if (next == null) return;
    e.preventDefault();
    e.stopPropagation();
    void selectQueueIndex(next, true, true);
  }

  function handleQueueOptionKey(e: KeyboardEvent, index: number) {
    if (e.key !== 'Enter' && e.key !== ' ') return;
    e.preventDefault();
    e.stopPropagation();
    void selectQueueIndex(index, true, true);
  }

  // Write a committed row back BY ID, never by an index snapshotted before the IPC: loadQueue() can
  // replace `queue` mid-flight, where the old index either stamps the decided row onto a DIFFERENT
  // undecided segment (hiding it from the reviewer) or, past the new end, punches an `undefined` hole
  // the rail's {#each} then throws on. Gone from the queue = drop the write, exactly as undo() does.
  function applyCommittedRow(seg: SpeechSegment) {
    const idx = queue.findIndex((s) => s.id === seg.id);
    if (idx >= 0) queue[idx] = seg;
  }

  // ── Keyboard handler ─────────────────────────────────────────────────────────
  function handleKey(e: KeyboardEvent) {
    if (isEditing) {
      if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        commitEdit();
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        void cancelEdit();
      }
      return;
    }
    // Never let a modifier chord (Ctrl+A select-all, Ctrl+F, Ctrl+K palette) fire a bare-key decision,
    // and never act while focus is in ANY editable element overlaid on the inbox (e.g. the command
    // palette input) — each mis-fire silently stamps a human adjudication on the current clip.
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const target = e.target as HTMLElement | null;
    if (
      (target?.tagName === 'BUTTON' || target?.tagName === 'A') &&
      (e.key === ' ' || e.key === 'Enter')
    ) {
      return;
    }
    if (
      target &&
      (target.tagName === 'INPUT' ||
        target.tagName === 'TEXTAREA' ||
        target.tagName === 'SELECT' ||
        target.isContentEditable)
    ) {
      return;
    }
    // Match on the PHYSICAL key (layout-independent): with the owner's Central Kurdish layout
    // active, e.key is 'ا'/'ب'/… and every letter shortcut below went dead until the OS layout was
    // toggled back — once per edited clip. physicalKey maps KeyA→'a', Digit1→'1' and falls back to
    // e.key for Space/Backspace/Escape/arrows.
    const key = physicalKey(e);
    switch (key) {
      case 'a':
        e.preventDefault();
        accept();
        break;
      case 'e':
        e.preventDefault();
        startEdit();
        break;
      case 'x':
        e.preventDefault();
        reject();
        break;
      case ' ':
        // True-10 audit: Space now means play/pause in BOTH review surfaces (it was SKIP here while
        // play/pause in ReviewMode — a reflexive Space silently skipped the current item, and the
        // inbox had no keyboard play at all while adjudicating biometric audio by ear).
        e.preventDefault();
        inboxPlaying = !inboxPlaying;
        break;
      case 'r':
        e.preventDefault();
        inboxCurrentTime = inboxRange.startTime;
        inboxPlaying = true;
        break;
      case 's':
        e.preventDefault();
        void skip();
        break;
      case 'f':
        e.preventDefault();
        flag();
        break;
      case 'Backspace':
        e.preventDefault();
        undo();
        break;
      case 'Escape':
        e.preventDefault();
        void requestClose();
        break;
      case 'n':
      case 'ArrowRight':
      case 'ArrowDown':
        // Non-destructive revisit navigation (true-10 audit: the inbox forced a mouse rail-click for
        // any move that wasn't a decision — or a destructive Backspace-undo to go back).
        e.preventDefault();
        void selectQueueIndex(currentIndex + 1, true, true);
        break;
      case 'p':
      case 'ArrowLeft':
      case 'ArrowUp':
        e.preventDefault();
        void selectQueueIndex(currentIndex - 1, true, true);
        break;
      default:
        if (key >= '1' && key <= '9') {
          const idx = parseInt(key) - 1;
          if (idx < queue.length) {
            void selectQueueIndex(idx, true, true);
          }
        }
    }
  }

  const unregisterDraftFlusher = registerReviewDraftFlusher(flushActiveReviewDraft);

  onMount(() => {
    void loadQueue();
    // Round-23 #12: reflect the REAL backend autonomy level on the dial, and hold the loaded settings
    // so the dial can persist changes and the cloud-T2 consent state can be surfaced.
    api
      .getSettings()
      .then((s) => {
        settings = s;
        autonomyLevel = s.juryAutonomyLevel ?? 'propose';
      })
      .catch(() => {
        /* leave the optimistic default; the dial just won't persist until settings load */
      });
    window.addEventListener('keydown', handleKey);
  });

  onDestroy(() => {
    window.removeEventListener('keydown', handleKey);
    clearDraftSaveTimer();
    void flushActiveReviewDraft().catch(() => undefined);
    unregisterDraftFlusher();
  });
</script>

<!-- ── Root container ──────────────────────────────────────────────────────── -->
<div class="inbox-root" role="dialog" aria-modal="true" aria-labelledby="review-inbox-title">
  <ReviewInboxHeader
    {pendingCount}
    {isRunningJury}
    localOnly={!!settings && !settings.juryCloudOptIn}
    {autonomyLevel}
    {closePending}
    onRunJury={() => void triggerJuryPipeline()}
    onSetAutonomy={(level) => void setAutonomy(level)}
    onClose={() => void requestClose()}
  />

  {#if isLoading}
    <div class="inbox-loading">
      <span class="spinner"></span>
      {$t('inbox.loadingQueue')}
    </div>
  {:else if loadError}
    <div class="inbox-empty" role="alert" data-testid="review-inbox-load-error">
      <h3>{$t('inbox.loadErrorTitle')}</h3>
      <p>{loadError}</p>
      <button class="btn btn-primary" onclick={loadQueue}>{$t('inbox.retry')}</button>
    </div>
  {:else if queue.length === 0}
    <div class="inbox-empty">
      <h3>{$t('inbox.zero')}</h3>
      <p>{$t('inbox.zeroHint')}</p>
      <div class="empty-actions">
        <button
          class="btn btn-primary"
          onclick={triggerJuryPipeline}
          disabled={isRunningJury}
          aria-describedby={isRunningJury ? 'inbox-empty-jury-disabled-reason' : undefined}
        >
          {isRunningJury ? $t('inbox.runningJury') : $t('inbox.runJuryPipeline')}
        </button>
        {#if isRunningJury}
          <span id="inbox-empty-jury-disabled-reason" class="sr-only">
            {$t('inbox.disabled.juryRunning')}
          </span>
        {/if}
        <button class="btn btn-secondary" onclick={loadQueue}>{$t('inbox.refresh')}</button>
      </div>
    </div>
  {:else}
    <div class="inbox-body">
      <ReviewInboxQueueRail
        {queue}
        {currentIndex}
        {current}
        {queueTotal}
        {nextCursor}
        {isLoadingMore}
        {loadMoreError}
        {evictedCount}
        {activeQueueAnnouncement}
        bind:queueListbox
        bandColor={(segment) =>
          confidenceBand(segment.agreementScore, $t, hasPoorAudio(segment)).color}
        optionId={queueOptionId}
        optionLabel={queueOptionLabel}
        onListboxKey={handleQueueListboxKey}
        onOptionKey={handleQueueOptionKey}
        onSelect={(selectedIndex) => void selectQueueIndex(selectedIndex, true, true)}
        onLoadMore={() => void loadMoreQueue()}
        onReloadStart={() => void loadQueue()}
      />

      <!-- Focus Card -->
      {#if current}
        {@const band = confidenceBand(current.agreementScore, $t, hasPoorAudio(current))}
        <article class="focus-card" aria-label={$t('inbox.segmentQueue')}>
          <!-- Segment ID + meta -->
          <div class="card-meta">
            <span class="meta-id"><bdi>{current.id.slice(0, 16)}</bdi></span>
            <span class="meta-dur"
              ><bdi
                >{$t('inbox.durationSeconds', {
                  seconds: String(Math.round(current.durationMs / 1000)),
                })}</bdi
              ></span
            >
            {#if current.speakerId}
              <span class="meta-speaker"><bdi>{current.speakerId}</bdi></span>
            {/if}
          </div>

          <!-- Audio playback (LTR always). Round-23 #13: a reviewer must be able to HEAR the clip before
               adjudicating a biometric Kurdish transcript — the old static filename stub offered no way
               to listen, yet Accept stamps a human-verified label. Bounded to THIS segment's window
               (inboxRange) so Play hears only this clip, not the whole file, and keyed on the segment id
               so the player re-resolves cleanly (no cross-segment audio bleed) as the queue is navigated. -->
          <div class="waveform-zone" dir="ltr" aria-label={$t('inbox.audioPlayback')}>
            {#if current.audioPath}
              {#key `${current.id}\0${String(currentRevision)}`}
                <!-- True-10 audit: honor the autoplay setting (was hardcoded off) — advancing the
                     queue auto-plays the clip so adjudication needs zero play clicks. -->
                <AudioPlayer
                  bind:this={inboxAudioPlayer}
                  audioPath={current.audioPath}
                  clipKey={current.id}
                  startTime={inboxRange.startTime}
                  endTime={inboxRange.endTime}
                  autoplay={settings?.autoplaySegments ?? false}
                  bind:playing={inboxPlaying}
                  bind:currentTime={inboxCurrentTime}
                  bind:heardMs={inboxHeardMs}
                  bind:duration={inboxDuration}
                  bind:playbackReceiptId={inboxPlaybackReceiptId}
                  bind:playbackMediaGrantId={inboxPlaybackMediaGrantId}
                  bind:playbackClipDurationMs={inboxPlaybackClipDurationMs}
                  bind:heardIntervals={inboxHeardIntervals}
                  bind:audioError
                  requirePlaybackProof={true}
                  expectedRevision={currentRevision}
                />
              {/key}
              {#if audioError}
                <fieldset
                  class="technical-unusable"
                  data-testid="inbox-technical-unusable"
                  dir="auto"
                >
                  <legend>{$t('review.unusable.title')}</legend>
                  <p id="inbox-unusable-help">{$t('review.unusable.help')}</p>
                  <label for="inbox-unusable-reason">
                    {$t('review.unusable.reasonLabel')}
                    <select
                      id="inbox-unusable-reason"
                      bind:value={technicalUnusableReason}
                      disabled={isSubmitting}
                      aria-describedby="inbox-unusable-help"
                    >
                      <option value="">{$t('review.unusable.reasonPlaceholder')}</option>
                      {#each technicalUnusableReasons as reason}
                        <option value={reason}>{$t(`review.unusable.reason.${reason}`)}</option>
                      {/each}
                    </select>
                  </label>
                  <button
                    type="button"
                    class="btn btn-secondary"
                    onclick={markTechnicallyUnusable}
                    disabled={isSubmitting ||
                      isEditing ||
                      !!draftAuthorityBlockedKey ||
                      !technicalUnusableReason}
                    aria-describedby="inbox-unusable-help"
                  >
                    {isSubmitting ? $t('review.unusable.marking') : $t('review.unusable.mark')}
                  </button>
                </fieldset>
              {/if}
              <div class="waveform-stub">
                <bdi>{current.audioPath?.split(/[\\/]/).pop() ?? $t('inbox.audioFallback')}</bdi>
              </div>
            {:else}
              <div class="waveform-stub"><bdi>{$t('inbox.noAudio')}</bdi></div>
            {/if}
          </div>

          <!-- Hypotheses section (RTL for Kurdish text) -->
          <section class="hyp-section">
            <h3 class="section-label">{$t('inbox.hypotheses')}</h3>
            <div class="hyp-raw" dir="rtl" lang="ckb">
              <span class="hyp-label-inline">{$t('rawAsr')}:</span>
              <span class="hyp-text">{current.rawTranscript}</span>
            </div>
            {#if current.normalizedTranscript && current.normalizedTranscript !== current.rawTranscript}
              <div class="hyp-norm" dir="rtl" lang="ckb">
                <span class="hyp-label-inline">{$t('normalized')}:</span>
                <span class="hyp-text">{current.normalizedTranscript}</span>
              </div>
            {/if}
          </section>

          <!-- Jury verdict (RTL) -->
          {#if current.verdictTranscript}
            <section class="verdict-section">
              <h3 class="section-label">{$t('inbox.juryProposes')}</h3>
              <div class="verdict-text" dir="rtl" lang="ckb">{current.verdictTranscript}</div>
            </section>
          {/if}

          <!-- Evidence & reasoning -->
          {#if current.rationale || current.evidenceJson}
            <section class="rationale-section">
              <h3 class="section-label">{$t('inbox.rationale')}</h3>
              <details class="rationale-details" open>
                <summary>{$t('inbox.evidenceReasoning')}</summary>
                {#if current.rationale}
                  <p class="rationale-text">{current.rationale}</p>
                {/if}
                {#if current.evidenceJson}
                  <pre class="evidence-pre">{safeEvidence(current.evidenceJson)}</pre>
                {/if}
              </details>
            </section>
          {/if}

          <!-- Confidence band -->
          <div class="confidence-strip" style="border-left-color:{band.color}">
            <span class="conf-label">{band.label}</span>
          </div>

          {#if draftLoadError}
            <div class="draft-state draft-error" role="alert">
              <p>{draftLoadError}</p>
              <button type="button" class="btn btn-secondary" onclick={() => void retryDraftLoad()}>
                {$t('inbox.draft.retry')}
              </button>
            </div>
          {:else if draftConflict}
            <div class="draft-state draft-conflict" role="alert">
              <h3>{$t('review.draftConflictTitle')}</h3>
              <p>{$t('review.draftConflictHint')}</p>
              <div class="draft-comparison">
                <section>
                  <h4>{$t('review.serverTruth')}</h4>
                  <p dir="rtl" lang="ckb">{draftBaseline}</p>
                </section>
                <section>
                  <h4>{$t('review.localDraft')}</h4>
                  <time dir="ltr">{draftConflict.updatedAt}</time>
                  <p dir="rtl" lang="ckb">{draftConflict.text}</p>
                </section>
              </div>
              <button type="button" class="btn btn-primary" onclick={useConflictingDraft}>
                {$t('review.useLocalDraft')}
              </button>
            </div>
          {/if}

          <div class="draft-status" aria-live="polite">
            {#if draftSaving}
              {$t('review.draftSaving')}
            {:else if draftSaveFailed}
              <span class="draft-error-text">{$t('review.draftSaveFailedHint')}</span>
            {:else if draftRecovered}
              {$t('review.draftRecovered')}
            {/if}
          </div>

          <!-- Edit area (shown when e pressed) -->
          {#if isEditing}
            <div class="edit-area">
              <label class="edit-label" for="edit-textarea">{$t('inbox.editLabel')}</label>
              <textarea
                id="edit-textarea"
                class="edit-textarea"
                dir="rtl"
                lang="ckb"
                value={editText}
                bind:this={editTextarea}
                oninput={(event) => handleEditInput(event.currentTarget.value)}
                disabled={isSubmitting}
                rows={3}
              ></textarea>
              <div class="edit-actions">
                <button
                  class="btn btn-primary"
                  onclick={commitEdit}
                  disabled={!!saveEditDisabledKey}
                  aria-describedby={saveEditDisabledKey
                    ? 'inbox-save-edit-disabled-reason'
                    : undefined}>{$t('inbox.saveEdit')}</button
                >
                {#if saveEditDisabledKey}
                  <span id="inbox-save-edit-disabled-reason" class="sr-only">
                    {$t(saveEditDisabledKey)}
                  </span>
                {/if}
                <button class="btn btn-secondary" onclick={() => void cancelEdit()}
                  >{$t('inbox.cancelEdit')}</button
                >
              </div>
            </div>
          {/if}

          <ReviewInboxActionBar
            {acceptDisabledKey}
            {editDisabledKey}
            {rejectDisabledKey}
            {skipDisabledKey}
            {flagDisabledKey}
            {undoDisabledKey}
            onAccept={() => void accept()}
            onEdit={() => void startEdit()}
            onReject={() => void reject()}
            onSkip={() => void skip()}
            onFlag={() => void flag()}
            onUndo={() => void undo()}
          />

          {#if statusMsg}
            <div class="status-bar" role="status" aria-live="polite">{statusMsg}</div>
          {/if}
        </article>
      {/if}
    </div>
  {/if}
</div>

<style>
  /* ── Root ──────────────────────────────────────────────────────────────────── */
  .inbox-root {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-width: 0;
    background: var(--app-bg);
    color: var(--text);
    font-family: var(--font-sans);
    border-radius: 12px;
    overflow: hidden;
  }

  /* ── Loading / Empty ─────────────────────────────────────────────────────────── */
  .inbox-loading,
  .inbox-empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    flex: 1;
    gap: 12px;
    color: var(--text-muted);
    font-size: 0.9rem;
  }
  .inbox-empty h3 {
    margin: 0;
    color: var(--text);
  }
  .inbox-empty p {
    margin: 0;
    text-align: center;
    max-width: 300px;
  }
  .empty-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 10px;
    width: 100%;
    margin-top: 10px;
  }
  .spinner {
    display: inline-block;
    width: 18px;
    height: 18px;
    border: 2px solid currentColor;
    border-top-color: transparent;
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  /* ── Body ───────────────────────────────────────────────────────────────────── */
  .inbox-body {
    display: flex;
    flex: 1;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }

  .draft-error-text {
    color: var(--danger);
  }

  /* ── Focus Card ─────────────────────────────────────────────────────────────── */
  .focus-card {
    flex: 1;
    min-width: 0;
    min-height: 0;
    overflow-y: auto;
    padding: 20px 24px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  /* ── Meta ────────────────────────────────────────────────────────────────────── */
  .card-meta {
    display: flex;
    gap: 10px;
    align-items: center;
    flex-wrap: wrap;
  }
  .meta-id,
  .meta-dur,
  .meta-speaker {
    background: var(--surface-2);
    border: 1px solid var(--border);
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 0.7rem;
    font-family: var(--font-mono);
    color: var(--text-muted);
  }

  /* ── Waveform zone ───────────────────────────────────────────────────────────── */
  .waveform-zone {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 12px 16px;
    font-size: 0.8rem;
    color: var(--text-muted);
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
    padding: 10px;
    border: 1px solid color-mix(in srgb, var(--warning) 45%, transparent);
    border-radius: 8px;
    background: color-mix(in srgb, var(--warning) 10%, transparent);
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
    color: var(--text);
    padding: 6px 8px;
  }

  /* ── Sections ────────────────────────────────────────────────────────────────── */
  .hyp-section,
  .verdict-section,
  .rationale-section {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .section-label {
    margin: 0;
    font-size: 0.72rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .hyp-raw,
  .hyp-norm {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 10px 14px;
    font-family: var(--font-kurdish);
    font-size: 1.05rem;
    line-height: 1.9;
    color: var(--text);
    text-align: start;
  }
  .hyp-label-inline {
    font-size: 0.65rem;
    color: var(--text-muted);
    font-family: var(--font-mono);
    display: inline-block;
    margin-inline-end: 8px;
  }
  .hyp-text {
    direction: rtl;
    /* isolate, not embed: the transcript sits inline after an LTR-ish "Raw ASR:" label in an RTL
       block. `embed` still lets a transcript that STARTS with a Latin token or digit reorder across
       the label boundary (colon/label jump to the wrong side). `isolate` gives the transcript its
       own bidi context so it can never reflow the label — same isolation the <bdi> model-name spans use. */
    unicode-bidi: isolate;
  }

  .verdict-text {
    background: var(--accent-soft);
    border: 1px solid color-mix(in srgb, var(--accent) 35%, transparent);
    border-radius: 8px;
    padding: 12px 16px;
    font-family: var(--font-kurdish);
    font-size: 1.1rem;
    line-height: 1.9;
    color: var(--text);
    text-align: start;
  }

  /* ── Rationale ────────────────────────────────────────────────────────────────── */
  .rationale-details {
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px 12px;
    font-size: 0.8rem;
  }
  .rationale-details summary {
    cursor: pointer;
    color: var(--text-muted);
  }
  .rationale-text {
    color: var(--text-muted);
    margin: 6px 0 0;
    line-height: 1.6;
  }
  .evidence-pre {
    background: var(--surface-inset);
    border-radius: 4px;
    padding: 8px;
    font-size: 0.7rem;
    color: var(--text-muted);
    overflow-x: auto;
    white-space: pre-wrap;
    word-break: break-all;
  }

  /* ── Confidence strip ─────────────────────────────────────────────────────────── */
  .confidence-strip {
    display: flex;
    align-items: center;
    gap: 8px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-left-width: 3px;
    border-radius: 6px;
    padding: 8px 14px;
    font-size: 0.8rem;
    color: var(--text);
  }
  .conf-label {
    flex: 1;
  }

  .draft-state {
    display: flex;
    flex-direction: column;
    gap: 8px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface-2);
    padding: 12px 16px;
    font-size: 0.8rem;
  }
  .draft-state h3,
  .draft-state h4,
  .draft-state p {
    margin: 0;
  }
  .draft-error {
    border-color: var(--danger);
  }
  .draft-conflict {
    border-color: var(--warning);
  }
  .draft-comparison {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px;
  }
  .draft-comparison section {
    min-width: 0;
    border-radius: 6px;
    background: var(--surface-inset);
    padding: 8px;
  }
  .draft-comparison p {
    margin-top: 6px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .draft-status {
    min-height: 1.25rem;
    font-size: 0.72rem;
    color: var(--text-muted);
  }

  /* ── Edit area ───────────────────────────────────────────────────────────────── */
  .edit-area {
    display: flex;
    flex-direction: column;
    gap: 8px;
    background: var(--surface-2);
    border: 1px solid var(--accent);
    border-radius: 8px;
    padding: 12px 16px;
  }
  .edit-label {
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  .edit-textarea {
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text);
    padding: 8px 12px;
    resize: vertical;
    font-family: var(--font-kurdish);
    font-size: 1rem;
    line-height: 1.9;
    direction: rtl;
    text-align: start;
    width: 100%;
    box-sizing: border-box;
  }
  .edit-actions {
    display: flex;
    gap: 8px;
  }

  /* ── Status bar ─────────────────────────────────────────────────────────────── */
  .status-bar {
    text-align: center;
    font-size: 0.78rem;
    color: var(--accent);
    padding: 4px 0;
    animation: fadeIn 0.2s ease;
  }
  @keyframes fadeIn {
    from {
      opacity: 0;
      transform: translateY(4px);
    }
    to {
      opacity: 1;
    }
  }

  /* WCAG reflow: at a 320 CSS-pixel viewport the App overlay leaves roughly 272px after padding.
     Stack the rail above the card and let header controls form deliberate rows; nothing is clipped
     behind the root's overflow boundary, while the queue remains a one-axis scroll region. */
  @media (max-width: 480px) {
    .inbox-root {
      border-radius: 8px;
    }
    .inbox-body {
      flex-direction: column;
    }
    .draft-comparison {
      grid-template-columns: minmax(0, 1fr);
    }
    .focus-card {
      width: 100%;
      padding: 12px;
    }
    .inbox-loading,
    .inbox-empty {
      min-width: 0;
      padding: 16px;
    }
    .empty-actions {
      flex-direction: column;
    }
    .empty-actions .btn {
      width: 100%;
    }
  }
</style>
