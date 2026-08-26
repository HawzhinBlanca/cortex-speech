<script lang="ts">
  import { onDestroy, tick } from 'svelte';
  import { get } from 'svelte/store';
  import {
    segments,
    selectedSegmentId,
    searchQuery,
    refreshSegmentStats,
  } from './stores/segmentStore';
  import * as api from './commands';
  import { notifications } from './stores/notificationStore';
  import { settings } from './stores/settingsStore';
  import { showReviewInbox, showConfirmDialog, isProcessing } from './stores/uiStore';
  import { physicalKey } from './keyboard';
  import { t, type TranslationKey } from './i18n';
  import Waveform from './Waveform.svelte';
  import AudioPlayer from './AudioPlayer.svelte';
  import EmptyState from './EmptyState.svelte';
  import ReviewActionBar from './ReviewActionBar.svelte';
  import ReviewDraftRecovery from './ReviewDraftRecovery.svelte';
  import ReviewWordStrip from './ReviewWordStrip.svelte';
  import {
    parseWordTimestamps,
    parseSourceMeta,
    chunkPlaybackRange,
    segmentSourceFilename,
    segmentChunkLabel,
  } from './alignment';
  import { wordPlayBounds, replaceWordToken } from './wordEdit';
  import { isPlaceholderTranscript } from './segmentQuality';
  import { revealReviewCompletion } from './reviewCompletion';
  import { parseEscalationEvidence, reasonLabelKey, reasonTone } from './reasonCodes';
  import { formatPublicErrorReference } from './errorText';
  import { registerReviewDraftFlusher } from './reviewDraftFlush';
  import {
    ReviewDraftWriteCoordinator,
    type RevisionBoundReviewDraftIntent,
  } from './reviewDraftWriteCoordinator';
  import { ReviewCommitOperationLedger, type ReviewCommitIntent } from './reviewCommitOperation';
  import {
    ReviewPlaybackAttemptLedger,
    hasSufficientReviewPlayback,
    isProvenUncommittedPlaybackFinalization,
  } from './reviewPlaybackAttempt';
  import { isCommittedReviewFor } from './reviewCommitResult';
  import type { SpeechSegment, WordTimestamp } from './types';
  import type { ReviewDraftV1 } from './commands';

  interface Props {
    // Pro next-steps surfaced when the whole queue is reviewed (wired by App to its export / exit).
    onExport?: () => void;
    onDone?: () => void;
  }
  let { onExport, onDone }: Props = $props();

  // M2.5/P1.4: suspect-first queue toggle. When on, the pending group is reordered by the backend's
  // suspect ranking (escalated first, then lowest agent confidence, then chronological) so the reviewer
  // lands on the riskiest clips first. Off by default — the plain pending-first order is unchanged.
  let suspectFirst = $state(false);
  let reviewRows = $state<SpeechSegment[]>([]);
  let reviewRevisions = $state<Record<string, number>>({});
  let reviewEligibility = $state<
    Record<string, { eligible: boolean; disabledReason: string | null }>
  >({});
  let reviewCursor = $state<string | null>(null);
  let reviewTotal = $state(0);
  let reviewInitialTotal = $state(0);
  let reviewCorpusTotal = $state(0);
  let reviewInitiallyVerified = $state(0);
  let reviewLoading = $state(false);
  let reviewLoadError = $state<string | null>(null);
  let hydratedReviewIds = $state<Set<string>>(new Set());
  type HydrationAttempt = {
    generation: number;
    baseRevision: number;
    promise: Promise<void>;
  };
  const hydrationInFlight = new Map<string, HydrationAttempt>();
  let reviewLoadKey = '';
  let reviewGeneration = 0;
  const commitOperations = new ReviewCommitOperationLedger();
  const playbackAttempts = new ReviewPlaybackAttemptLedger();

  function committedEffectId(
    segmentId: string,
    baseRevision: number,
    commit: {
      segmentId: string;
      committedRevision: number;
      decisionId: string;
    },
  ): number | null {
    if (!isCommittedReviewFor(commit, segmentId, baseRevision)) {
      notifications.error($t('notifications.loadSegmentsFailed'));
      void loadReviewPage(true);
      return null;
    }
    const effectEventId = api.reviewEffectId(commit.decisionId);
    if (effectEventId === null) {
      notifications.error($t('notifications.loadSegmentsFailed'));
      void loadReviewPage(true);
      return null;
    }
    return effectEventId;
  }

  async function hydrateReviewRow(id: string, force = false) {
    if (!force && hydratedReviewIds.has(id)) return;
    const generation = reviewGeneration;
    const baseRevision = reviewRevisions[id];
    if (!Number.isSafeInteger(baseRevision) || baseRevision < 0) {
      throw new Error('review row hydration requires the exact rendered revision');
    }
    const existing = hydrationInFlight.get(id);
    if (existing && existing.generation === generation && existing.baseRevision === baseRevision) {
      return existing.promise;
    }

    const hydration = (async () => {
      const full = await api.getSegment(id);
      // A reset can serve the SAME id at a newer revision while this unversioned full-row hydration is
      // still in flight. Pairing that stale row with the newer page's CAS token would let a reviewer
      // overwrite truth they never saw. Only the exact page generation + revision that launched this
      // request may install it; the replacement page starts its own hydration attempt.
      if (
        generation !== reviewGeneration ||
        reviewRevisions[id] !== baseRevision ||
        !reviewRows.some((row) => row.id === id)
      ) {
        return;
      }
      reviewRows = reviewRows.map((row) => (row.id === id ? full : row));
      segments.update((rows) => rows.map((row) => (row.id === id ? full : row)));
      hydratedReviewIds = new Set([...hydratedReviewIds, id]);
    })();
    const attempt = { generation, baseRevision, promise: hydration };
    hydrationInFlight.set(id, attempt);
    try {
      await hydration;
    } finally {
      if (hydrationInFlight.get(id) === attempt) hydrationInFlight.delete(id);
    }
  }

  async function loadReviewPage(reset: boolean) {
    if (!reset && (reviewLoading || !reviewCursor)) return;
    const generation = reset ? ++reviewGeneration : reviewGeneration;
    const cursor = reset ? null : reviewCursor;
    const query = $searchQuery.trim() || null;
    if (reset) {
      // A new scope/order must fail closed. Keeping the previous scope actionable while its
      // replacement loads can record a decision against a clip the reviewer can no longer see in
      // context, and a failed reset must never masquerade as the successful all-done state.
      reviewRows = [];
      reviewRevisions = {};
      reviewEligibility = {};
      reviewCursor = null;
      reviewTotal = 0;
      reviewInitialTotal = 0;
      reviewCorpusTotal = 0;
      reviewInitiallyVerified = 0;
      reviewLoadError = null;
      hydratedReviewIds = new Set();
      index = 0;
    }
    reviewLoading = true;
    try {
      const statsPromise =
        reset && !query ? api.getDatasetStats().catch(() => null) : Promise.resolve(null);
      let pageRows: SpeechSegment[];
      let pageRevisions: Record<string, number>;
      let pageEligibility: Record<string, { eligible: boolean; disabledReason: string | null }>;
      let pageNextCursor: string | null;
      let pageTotal: number;
      let pageFocusNarrowed: boolean;
      if (suspectFirst) {
        // The legacy suspect ranking remains during domain-by-domain migration, but its additive
        // revision map comes from the SAME SQLite rows and therefore still authorizes typed commits.
        const page = await api.getSegmentsPage({
          verified: false,
          query,
          sort: 'suspectFirst',
          limit: 100,
          cursor,
          focused: true,
        });
        pageRows = page.items;
        pageRevisions = page.revisions ?? {};
        // The suspect-first endpoint predates ReviewItemV1. Preserve the same fail-closed readiness
        // rule locally until this optional ordering moves behind the versioned server contract.
        pageEligibility = Object.fromEntries(
          page.items.map((item) => {
            const eligible =
              Number.isSafeInteger(pageRevisions[item.id]) &&
              pageRevisions[item.id] >= 0 &&
              !isPlaceholderTranscript(item.rawTranscript);
            return [
              item.id,
              {
                eligible,
                disabledReason: eligible ? null : 'TRANSCRIPT_NOT_READY',
              },
            ];
          }),
        );
        pageNextCursor = page.nextCursor;
        pageTotal = page.total;
        pageFocusNarrowed = page.focusNarrowed === true;
      } else {
        const scope = query ? ({ kind: 'search', query } as const) : ({ kind: 'pending' } as const);
        const page = await api.getReviewPageV1(scope, cursor, 100);
        pageRows = page.items.map((item) => item.segment);
        pageRevisions = Object.fromEntries(
          page.items.map((item) => [item.segment.id, item.baseRevision]),
        );
        pageEligibility = Object.fromEntries(
          page.items.map((item) => [
            item.segment.id,
            { eligible: item.eligible, disabledReason: item.disabledReason },
          ]),
        );
        pageNextCursor = page.nextCursor;
        pageTotal = page.total;
        pageFocusNarrowed = page.focusNarrowed;
      }
      const stats = await statsPromise;
      if (generation !== reviewGeneration) return;
      reviewLoadError = null;
      reviewRows = reset ? pageRows : [...reviewRows, ...pageRows];
      reviewRevisions = reset ? pageRevisions : { ...reviewRevisions, ...pageRevisions };
      reviewEligibility = reset ? pageEligibility : { ...reviewEligibility, ...pageEligibility };
      reviewCursor = pageNextCursor;
      focusNarrowed = pageFocusNarrowed;
      if (reset) {
        reviewTotal = pageTotal;
        reviewInitialTotal = pageTotal;
        reviewCorpusTotal = stats?.totalSegments ?? pageTotal;
        reviewInitiallyVerified = stats?.verifiedCount ?? 0;
        index = 0;
      }
    } catch (error) {
      if (generation !== reviewGeneration) return;
      reviewLoadError = formatPublicErrorReference(error) ?? $t('errors.unknown');
      if (reset) {
        reviewRows = [];
        reviewRevisions = {};
        reviewEligibility = {};
        reviewCursor = null;
        reviewTotal = 0;
        reviewInitialTotal = 0;
      }
      notifications.error($t('notifications.loadSegmentsFailed'), { cause: error });
    } finally {
      if (generation === reviewGeneration) reviewLoading = false;
    }
  }

  $effect(() => {
    const key = `${$searchQuery.trim()}\0${suspectFirst ? 'suspect' : 'oldest'}`;
    if (key === reviewLoadKey) return;
    reviewLoadKey = key;
    void loadReviewPage(true);
  });

  async function toggleSuspectFirst() {
    suspectFirst = !suspectFirst;
  }

  // True-10 audit: a curate-mode SEARCH now scopes the review queue (review one source file, one
  // speaker, or any search subset) — with an explicit banner so the scope is never silent. Only the
  // search scopes; the verified-filter is ignored here because review mode has its own
  // pending-first ordering (a verified-only filter would render the queue permanently "all done").
  const searchScoped = $derived($searchQuery.trim().length > 0);
  // A voice focus narrows this queue to one speaker's clips, so — exactly like a search — the queue
  // is a SUBSET and corpus-wide progress claims are lies about it. Set from the server's own answer
  // (it alone knows whether a focus file was in force for this fetch), never inferred from the flag
  // we sent. Review 2026-08-20: draining 1,318 focused clips announced the whole 15,262-clip library
  // as reviewed, because the completion banner only ever excluded the SEARCH subset.
  let focusNarrowed = $state(false);
  const subsetScoped = $derived(searchScoped || focusNarrowed);

  // Simple, focused review queue: one clip at a time. Pending (unverified) first,
  // then the rest — so a reviewer always lands on work that needs doing.
  // searchScopedSegments (NOT filteredSegments) enforces the search-only contract above: the
  // curate "✓ Verified" chip must never leak in and empty the queue (true-10 audit).
  const queue = $derived(reviewRows);

  let index = $state(0);
  // M2.6/P1.5: on first queue availability, resume at the restored session cursor (the last segment
  // the reviewer acted on) instead of always restarting at 0. One-shot — never fights later navigation.
  let cursorRestored = $state(false);
  $effect(() => {
    if (cursorRestored || queue.length === 0) return;
    const targetId = $selectedSegmentId;
    if (targetId) {
      const pos = queue.findIndex((s) => s.id === targetId);
      // Land on the restored cursor only when it still needs work. A VERIFIED cursor (the last clip
      // the reviewer ACTED on) would reopen finished gold as the very first thing on screen — one
      // accidental keypress from destroying it (the 2026-07-14 live-test incident). Verified cursor →
      // start at the queue head (first pending) instead.
      if (pos >= 0 && !queue[pos].verified) index = pos;
    }
    cursorRestored = true;
  });
  // Paged rows deliberately omit alignment/evidence payloads. They must never become actionable until
  // get_segment has restored the full row: alignment_json carries the source chunk boundaries, and
  // aligning a lightweight row's null value would treat it as a whole-file clip and overwrite those
  // boundaries. This gate makes hydration part of the review-row state machine, not a best-effort race.
  const currentCandidate = $derived(queue[index] ?? null);
  const current = $derived(
    currentCandidate && hydratedReviewIds.has(currentCandidate.id) ? currentCandidate : null,
  );
  const currentEligibility = $derived(
    current
      ? (reviewEligibility[current.id] ?? {
          eligible: false,
          disabledReason: 'REVIEW_ELIGIBILITY_UNKNOWN',
        })
      : null,
  );
  const eligibilityBlocked = $derived(currentEligibility?.eligible !== true);

  function eligibilityReasonText(reason: string | null | undefined): string {
    return reason === 'TRANSCRIPT_NOT_READY'
      ? $t('review.transcriptNotReady')
      : $t('review.eligibilityUnavailable');
  }
  $effect(() => {
    const candidate = currentCandidate;
    if (!candidate || hydratedReviewIds.has(candidate.id)) return;
    void hydrateReviewRow(candidate.id).catch((error) => {
      reviewLoadError = formatPublicErrorReference(error) ?? $t('errors.unknown');
      notifications.error($t('notifications.loadSegmentsFailed'), { cause: error });
    });
  });
  // Audit 2026-08-05: the position counter above counts the QUEUE while the progress text counted the
  // whole CORPUS, so an active search silently split the denominator and the two lines contradicted
  // each other on screen. One call now produces both. `progress.allReviewed` stays corpus-scoped on
  // purpose — a fully-reviewed SEARCH SUBSET must never fire the completion banner. See
  // reviewProgress.ts, where that rule is unit-tested.
  const progress = $derived({
    done: subsetScoped
      ? Math.max(0, reviewInitialTotal - reviewTotal)
      : Math.min(
          reviewCorpusTotal,
          reviewInitiallyVerified + Math.max(0, reviewInitialTotal - reviewTotal),
        ),
    total: subsetScoped ? reviewInitialTotal : reviewCorpusTotal,
    percent:
      (subsetScoped ? reviewInitialTotal : reviewCorpusTotal) > 0
        ? Math.round(
            ((subsetScoped
              ? reviewInitialTotal - reviewTotal
              : reviewInitiallyVerified + reviewInitialTotal - reviewTotal) /
              (subsetScoped ? reviewInitialTotal : reviewCorpusTotal)) *
              100,
          )
        : 0,
    allReviewed: !subsetScoped && reviewCorpusTotal > 0 && reviewTotal === 0,
  });

  $effect(() => {
    if (reviewCursor && index >= queue.length - 10) void loadReviewPage(false);
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
  let waveformData = $state<number[]>([]);
  // Non-null ONLY when the decode failed. Distinguishes "could not read the audio" from a genuinely
  // quiet clip, which an empty array alone cannot.
  let waveformError = $state<string | null>(null);
  let currentTime = $state(0);
  let playerDuration = $state(0);
  let playing = $state(false);
  let saving = $state(false);
  // Bound from AudioPlayer: non-null means this clip's audio could not load/play, so no decision
  // surface may record a human verdict on it (audit find 2026-08-17).
  let audioError = $state<string | null>(null);
  const technicalUnusableReasons: readonly api.TechnicalUnusableReasonV1[] = [
    'decodeFailed',
    'missingFile',
    'permissionDenied',
    'corruptContainer',
  ];
  let technicalUnusableReason = $state<api.TechnicalUnusableReasonV1 | ''>('');
  let technicalUnusableForId = $state<string | null>(null);
  let technicalUnusableIntent = $state<api.MarkSegmentUnusableRequestV1 | null>(null);
  $effect(() => {
    const segmentId = current?.id ?? null;
    if (segmentId === technicalUnusableForId) return;
    technicalUnusableForId = segmentId;
    technicalUnusableReason = '';
    technicalUnusableIntent = null;
  });
  // Cumulative MEDIA time heard for the CURRENT clip; the player resets it on every
  // source change, so a previous clip's listen can never travel with the reviewer.
  let heardMs = $state(0);
  let playbackReceiptId = $state<string | null>(null);
  let playbackMediaGrantId = $state<string | null>(null);
  let playbackClipDurationMs = $state<number | null>(null);
  let heardIntervals = $state<readonly api.PlaybackIntervalV1[]>([]);
  let reviewAudioPlayer = $state<
    | {
        pauseAndSnapshot: () => {
          segmentId: string | null;
          segmentRevision: number | null;
          playbackReceiptId: string | null;
          mediaGrantId: string | null;
          clipDurationMs: number | null;
          intervals: readonly api.PlaybackIntervalV1[];
        };
        restartPlaybackAuthority: () => void;
      }
    | undefined
  >();
  let lastLoadedId = $state<string | null>(null);

  $effect(() => {
    if (!$showReviewInbox) return;
    // The inbox is a separate review authority. Retire this surface's player (the template unmounts it)
    // and clear parent-owned playback state before the foreground surface can issue a new receipt.
    playing = false;
    playbackReceiptId = null;
    playbackMediaGrantId = null;
    playbackClipDurationMs = null;
    heardIntervals = [];
    heardMs = 0;
  });

  async function finalizePlaybackAttempt(
    seg: SpeechSegment,
    baseRevision: number,
  ): Promise<string | null> {
    // Pause and snapshot inside AudioPlayer. Setting a bound parent flag is not synchronous evidence:
    // the child may still have one final media delta between its last timeupdate and the click.
    const authority = await reviewAudioPlayer?.pauseAndSnapshot();
    playing = false;
    if (
      !authority ||
      authority.segmentId !== seg.id ||
      authority.segmentRevision !== baseRevision
    ) {
      notifications.error($t('review.mustListen'));
      return null;
    }
    const alreadyFinalized = playbackAttempts.finalizedReceipt(seg.id, baseRevision);
    if (alreadyFinalized) return alreadyFinalized;
    const receiptId = authority.playbackReceiptId;
    const grantId = authority.mediaGrantId;
    const clipDurationMs = authority.clipDurationMs;
    const intervals = authority.intervals.map(({ startMs, endMs }) => ({ startMs, endMs }));
    if (
      !receiptId ||
      !grantId ||
      !Number.isSafeInteger(clipDurationMs) ||
      (clipDurationMs ?? 0) <= 0 ||
      !hasSufficientReviewPlayback(intervals, clipDurationMs as number)
    ) {
      notifications.error($t('review.mustListen'));
      return null;
    }
    const attempt = playbackAttempts.snapshot({
      segmentId: seg.id,
      baseRevision,
      playbackReceiptId: receiptId,
      mediaGrantId: grantId,
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
        throw new Error('playback receipt response identity mismatch');
      }
      playbackAttempts.markFinalized(seg.id, baseRevision, finalized.playbackReceiptId);
      return finalized.playbackReceiptId;
    } catch (error) {
      // Retire only a typed server attestation emitted before the receipt transaction commits. Every
      // other failure is ambiguous and keeps the first immutable union for byte-identical replay.
      if (isProvenUncommittedPlaybackFinalization(error)) {
        playbackAttempts.resolve(seg.id, baseRevision);
        reviewAudioPlayer?.restartPlaybackAuthority();
      }
      throw error;
    }
  }

  // Engines that actually produced this clip's draft, recorded (never inferred) and shown as an
  // honest provenance badge.
  //
  // The multi-model CONSENSUS card this call also used to feed was removed (2026-08-13): it was an
  // ability-weighted vote across engines measured at 19-40% CER on this corpus against a champion at
  // 10.6%, and the same fusion changed 0 of 135 clips when measured on the owner's own reviewed
  // audio. Its "Use draft" button was therefore a one-tap downgrade of the champion's text. The
  // per-clip model list it returns is still honest and still shown.
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
    if (!seg || retranscribing || saving || aligning || $isProcessing) return;
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
    if (!seg || retranscribing || saving || aligning || $isProcessing) return;
    retranscribing = true;
    try {
      const result = await api.transcribeSegment(seg.audioPath, seg.alignmentJson, seg.id);
      const text = result.text;
      // The champion command commits server-side after all enabled refinement succeeds. Reload that
      // authoritative row; never spread/upsert the UI snapshot captured before a long GPU call.
      const updated = await api.getSegment(seg.id);
      segments.update((list) => list.map((s) => (s.id === seg.id ? updated : s)));
      reviewRows = reviewRows.map((s) => (s.id === seg.id ? updated : s));
      notifications.success($t('review.retranscribed'));
      // The DB/store write above targets seg by id and is correct even if the reviewer navigated away
      // during the multi-second ASR await. But everything below mutates the CURRENTLY shown editor
      // (editText/lastLoadedOriginal/draftModels/alignment) — if navigation changed `current`
      // mid-flight, applying seg's draft here would put seg's MACHINE text into another clip's editor,
      // and a subsequent Save would persist it as THAT clip's human-verified gold: a wrong-segment gold
      // corruption (THE ONE LAW). Bail; seg's clip reloads its fresh draft + re-aligns when reopened.
      if (current?.id !== seg.id) return;
      editText = text;
      editedChips = {}; // a fresh draft replaces the transcript wholesale — drop stale chip fixes
      // The re-transcribed draft is the new baseline (not a "dirty" edit). Do NOT reset lastLoadedId —
      // the clip id is unchanged, so the load effect must stay a no-op; resetting it would re-run
      // loadConsensus and wipe the provenance badge we set just below.
      lastLoadedOriginal = text;
      // Use the provenance committed by the backend, never a UI-side model assumption.
      draftModels = updated.modelVersionId ? [updated.modelVersionId] : [];
      await ensureWordTimings(updated);
    } catch (e) {
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
      retranscribing = false;
    }
  }

  // "This draft is wrong" — mark the clip bad so it leaves the review queue and is excluded from
  // dataset export, but the audio + draft are KEPT (reversible: re-review to accept, or re-transcribe).
  // Records a human 'reject' decision (verdict = human_reject) which the export path already honors.
  async function markBad() {
    const seg = current;
    if (!seg || saving || retranscribing || aligning) return;
    if (draftAuthorityBlockedKey) {
      notifications.error($t(draftAuthorityBlockedKey));
      return;
    }
    if (dirty) {
      // Rejecting clears the revision-bound recovery draft in the human-decision transaction. Never
      // let Mark bad silently discard a correction that is still visible in the editor.
      notifications.error($t('review.rejectDisabledEdited'));
      return;
    }
    if (eligibilityBlocked) {
      notifications.error(eligibilityReasonText(currentEligibility?.disabledReason));
      return;
    }
    // A verdict on audio nobody could hear is indistinguishable downstream from a real listen, and
    // this is a VERBATIM corpus. Refuse rather than record it (audit find 2026-08-17).
    if (audioError) {
      notifications.error($t('review.cannotDecideWithoutAudio'));
      return;
    }
    const baseRevision = reviewRevisions[seg.id];
    if (!Number.isSafeInteger(baseRevision) || baseRevision < 0) {
      notifications.error($t('notifications.loadSegmentsFailed'));
      void loadReviewPage(true);
      return;
    }
    // No blocking confirm (true-10 audit): 'x' is undoable via Backspace now, so a native
    // window.confirm per press only broke the keyboard flow.
    saving = true;
    let commitIntent: ReviewCommitIntent;
    try {
      await flushActiveReviewDraft();
      if (
        current?.id !== seg.id ||
        reviewRevisions[seg.id] !== baseRevision ||
        draftAuthorityBlocked ||
        dirty
      ) {
        notifications.error($t('inbox.status.draftChangedDuringSave'));
        return;
      }
      // Same receipt the submit() path posts, for the same reason. A reject permanently removes
      // a clip from the corpus, so it is a verdict on the audio exactly as much as an accept is.
      const finalizedReceiptId = await finalizePlaybackAttempt(seg, baseRevision);
      if (!finalizedReceiptId) return;
      commitIntent = {
        segmentId: seg.id,
        baseRevision,
        decision: 'reject',
        transcript: null,
        reasonCode: null,
        playbackReceiptId: finalizedReceiptId,
      };
      const commit = await api.commitReviewV1({
        operationId: commitOperations.idFor(commitIntent),
        ...commitIntent,
      });
      const effectEventId = committedEffectId(seg.id, baseRevision, commit);
      if (effectEventId === null) return;
      commitOperations.resolve(commitIntent);
      playbackAttempts.resolve(seg.id, baseRevision);
      // Undo authority is the immutable database effect id, never this renderer's pre-save row.
      // Push only after the atomic decision commits, so a failed decision cannot create a phantom undo.
      undoHistory = [
        ...undoHistory,
        { id: seg.id, effectEventId, operationId: crypto.randomUUID() },
      ];
      segments.update((list) =>
        list.map((stored) =>
          stored.id === seg.id
            ? {
                ...stored,
                verified: true,
                verdict: 'human_reject',
                humanDecision: 'reject',
                verdictTranscript: commit.authoritativeTranscript,
              }
            : stored,
        ),
      );
      const visibleId = current?.id ?? null;
      reviewRows = reviewRows.filter((s) => s.id !== seg.id);
      const remainingRevisions = { ...reviewRevisions };
      delete remainingRevisions[seg.id];
      reviewRevisions = remainingRevisions;
      const remainingEligibility = { ...reviewEligibility };
      delete remainingEligibility[seg.id];
      reviewEligibility = remainingEligibility;
      reviewTotal = Math.max(0, reviewTotal - 1);
      void refreshSegmentStats();
      editCache.delete(seg.id);
      draftWrites.acknowledge({ kind: 'delete', segmentId: seg.id, baseRevision });
      notifications.success($t('review.markedBad'));
      if (visibleId === seg.id) advance();
      else {
        const visibleIndex = visibleId ? queue.findIndex((row) => row.id === visibleId) : -1;
        if (visibleIndex >= 0) index = visibleIndex;
      }
    } catch (e) {
      if (api.isCommandErrorV1(e, 'NO_PLAYBACK_EVIDENCE'))
        notifications.error($t('review.mustListen'));
      else {
        notifications.error($t('notifications.saveFailed'));
        if (api.isCommandErrorV1(e, 'STALE_REVISION')) {
          // A typed error alone is not a non-commit attestation: the transaction may have committed
          // before a later response-stage failure. Keep both exact retry ledgers until a verified
          // commit response resolves them; the refreshed authoritative revision naturally prevents
          // the old intent from being applied to a different row state.
          void loadReviewPage(true);
        }
      }
    } finally {
      saving = false;
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

  /**
   * Record a technical media disposition, never a human transcript verdict. This path deliberately
   * does not finalize or submit playback evidence: a missing/undecodable file cannot produce honest
   * listening proof. The exact operation id is retained across an uncertain response so Retry cannot
   * duplicate the durable effect or silently switch reasons.
   */
  async function markTechnicallyUnusable() {
    const seg = current;
    const reason = technicalUnusableReason;
    if (!seg || !audioError || !reason || saving || retranscribing || aligning) return;
    if (draftAuthorityBlockedKey) {
      notifications.error($t(draftAuthorityBlockedKey));
      return;
    }
    if (dirty) {
      notifications.error($t('review.rejectDisabledEdited'));
      return;
    }
    const baseRevision = reviewRevisions[seg.id];
    if (!Number.isSafeInteger(baseRevision) || baseRevision < 0) {
      notifications.error($t('review.unusable.authorityMissing'));
      return;
    }
    const matchingIntent =
      technicalUnusableIntent?.segmentId === seg.id &&
      technicalUnusableIntent.baseRevision === baseRevision &&
      technicalUnusableIntent.reason === reason
        ? technicalUnusableIntent
        : null;
    const request: api.MarkSegmentUnusableRequestV1 = matchingIntent ?? {
      operationId: crypto.randomUUID(),
      segmentId: seg.id,
      baseRevision,
      reason,
    };
    technicalUnusableIntent = request;
    saving = true;
    try {
      await flushActiveReviewDraft();
      if (
        current?.id !== seg.id ||
        reviewRevisions[seg.id] !== baseRevision ||
        technicalUnusableReason !== reason ||
        draftAuthorityBlocked ||
        dirty
      ) {
        notifications.error($t('inbox.status.draftChangedDuringSave'));
        return;
      }
      const response = await api.markSegmentUnusableV1(request);
      if (!isMarkedUnusableResponse(response, request)) {
        notifications.error($t('review.unusable.invalidResponse'));
        return;
      }
      technicalUnusableIntent = null;
      const visibleId = current?.id ?? null;
      if (visibleId === seg.id) {
        // The backend deleted this exact revision's crash-safe draft in the SAME transaction. Make
        // the outgoing-clip effect see a clean baseline so it cannot enqueue a post-commit re-save.
        lastLoadedOriginal = editText;
      }
      segments.update((list) =>
        list.map((stored) =>
          stored.id === seg.id ? { ...stored, escalated: true, verified: false } : stored,
        ),
      );
      reviewRows = reviewRows.filter((row) => row.id !== seg.id);
      const remainingRevisions = { ...reviewRevisions };
      delete remainingRevisions[seg.id];
      reviewRevisions = remainingRevisions;
      const remainingEligibility = { ...reviewEligibility };
      delete remainingEligibility[seg.id];
      reviewEligibility = remainingEligibility;
      hydratedReviewIds = new Set([...hydratedReviewIds].filter((id) => id !== seg.id));
      reviewTotal = Math.max(0, reviewTotal - 1);
      editCache.delete(seg.id);
      draftWrites.acknowledge({ kind: 'delete', segmentId: seg.id, baseRevision });
      void refreshSegmentStats();
      notifications.success($t('review.unusable.marked'));
      if (visibleId === seg.id) advance();
      else {
        const visibleIndex = visibleId ? queue.findIndex((row) => row.id === visibleId) : -1;
        if (visibleIndex >= 0) index = visibleIndex;
      }
    } catch (error) {
      notifications.error($t('review.unusable.markFailed'), {
        cause: error,
        publicDetail: api.reviewErrorMessage(error, $t('review.unusable.markFailedHint')),
      });
    } finally {
      saving = false;
    }
  }

  // The server owns the pre-decision snapshot. The renderer retains only the immutable effect id and
  // one stable operation UUID, so a retry after a lost response is idempotent and cannot restore
  // attacker-controlled or stale segment fields.
  let undoHistory = $state<{ id: string; effectEventId: number; operationId: string }[]>([]);

  async function undoLast() {
    const last = undoHistory[undoHistory.length - 1];
    if (!last || saving || retranscribing) return;
    saving = true;
    undoHistory = undoHistory.slice(0, -1);
    try {
      const outcome = await api.undoHumanDecision(last.effectEventId, last.operationId);
      if (outcome.status === 'conflict') {
        // Nothing was undone. Preserve the immutable token so the conflict cannot silently erase the
        // only retry/inspection handle for this exact decision.
        undoHistory = [...undoHistory, last];
        notifications.error($t('review.undoFailed'), {
          publicDetail: $t('inbox.error.undoDecisionConflict'),
        });
        return;
      }
      const restored = outcome.segment;
      reviewRevisions = { ...reviewRevisions, [last.id]: outcome.restoredRevision };
      reviewEligibility = {
        ...reviewEligibility,
        [last.id]: {
          eligible: !isPlaceholderTranscript(restored.rawTranscript),
          disabledReason: isPlaceholderTranscript(restored.rawTranscript)
            ? 'TRANSCRIPT_NOT_READY'
            : null,
        },
      };
      // The same segment id may re-enter the queue immediately. Force the load effect to consume the
      // authoritative restored fields instead of treating that id as the already-loaded decided row.
      editCache.delete(last.id);
      if (lastLoadedId === last.id) lastLoadedId = null;
      segments.update((list) => list.map((s) => (s.id === last.id ? restored : s)));
      const wasPending = reviewRows.some((s) => s.id === last.id);
      if (!restored.verified) {
        reviewRows = wasPending
          ? reviewRows.map((s) => (s.id === last.id ? restored : s))
          : [restored, ...reviewRows];
        if (!wasPending) reviewTotal += 1;
      } else if (wasPending) {
        reviewRows = reviewRows.filter((s) => s.id !== last.id);
        reviewTotal = Math.max(0, reviewTotal - 1);
      }
      hydratedReviewIds = new Set([...hydratedReviewIds, last.id]);
      void refreshSegmentStats();
      const idx = queue.findIndex((s) => s.id === last.id);
      if (idx >= 0) index = idx;
      notifications.success($t('review.undone'));
    } catch (e) {
      // Not undone — put the entry back so the undo stays retryable.
      undoHistory = [...undoHistory, last];
      notifications.error($t('review.undoFailed'), { cause: e });
    } finally {
      saving = false;
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
    if (!seg || cloudChecking || saving || retranscribing) return;
    cloudChecking = true;
    cloudCheck = null;
    try {
      const result = await api.runT2ForSegment(seg.id, $settings.llmApiKey);
      cloudCheck = { id: seg.id, result };
    } catch (e) {
      notifications.error($t('review.cloudCheckFailed'), { cause: e });
    } finally {
      cloudChecking = false;
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
    const clipT = currentTime - range.startTime;
    return words.findIndex((w) => clipT >= w.start && clipT < w.end);
  });
  const clipLength = $derived(
    range.endTime > range.startTime ? range.endTime - range.startTime : playerDuration,
  );
  const clipPosition = $derived(
    range.endTime > range.startTime
      ? Math.max(0, Math.min(currentTime - range.startTime, clipLength))
      : currentTime,
  );

  // The primary decision playback covers the complete database segment. Word chips can temporarily
  // narrow playback, but accepting a clip requires hearing the same full source span the backend
  // hashes and evaluates; silently skipping VAD padding could make an honest 85% receipt impossible
  // or hide a defect in the omitted audio.
  const SPOKEN_PAD = 0.12;
  const playStart = $derived(range.startTime);
  const playEnd = $derived(range.endTime);

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
    // VERBATIM LAW (2026-08-12): the reviewer corrects the human draft else the champion's verbatim
    // output — never the LLM-refined paraphrase column. A BLANK annotated column is ABSENT, not a
    // human draft: `??` only falls through on null, so an empty/whitespace annotated row masked the
    // champion raw draft and served the reviewer an empty editor (canon: human ▸ annotated ▸ raw).
    return seg.annotatedTranscript?.trim() ? seg.annotatedTranscript : (seg.rawTranscript ?? '');
  }

  // Plain (non-reactive) cache of in-progress edits keyed by segment id, so switching clips — via
  // prev/next OR a queue reorder from a concurrent store reload — never silently discards an unsaved
  // correction. Cleared per id on a successful save.
  const editCache = new Map<string, string>();
  let lastLoadedOriginal = '';
  let lastLoadedRevision: number | null = null;
  let draftReadyId = $state<string | null>(null);
  let draftConflict = $state<ReviewDraftV1 | null>(null);
  let draftLoadError = $state<string | null>(null);
  let draftRecovered = $state(false);
  let draftSaving = $state(false);
  let draftSaveFailed = $state(false);
  let draftLoadSeq = 0;
  const draftWrites = new ReviewDraftWriteCoordinator({
    save: api.saveReviewDraftV1,
    delete: api.deleteReviewDraftV1,
    onStateChange: (segmentId) => {
      if (current?.id === segmentId) draftSaving = draftWrites.isWriting(segmentId);
    },
    onWriteSucceeded: (intent) => {
      if (current?.id === intent.segmentId && !draftWrites.hasDesired(intent.segmentId)) {
        draftSaveFailed = false;
      }
    },
    onWriteFailed: (intent, error) => {
      if (current?.id !== intent.segmentId) return;
      draftSaveFailed = true;
      notifications.error($t('review.draftSaveFailed'), {
        cause: error,
        publicDetail: api.reviewErrorMessage(error, $t('review.draftSaveFailedHint')),
      });
    },
  });
  const draftAuthorityBlockedKey = $derived<TranslationKey | null>(
    !current
      ? null
      : draftLoadError
        ? 'inbox.disabled.draftUnavailable'
        : draftReadyId !== current.id
          ? 'inbox.disabled.draftLoading'
          : draftConflict
            ? 'inbox.disabled.draftConflict'
            : null,
  );
  const draftAuthorityBlocked = $derived(draftAuthorityBlockedKey !== null);
  const decisionBlocked = $derived(
    eligibilityBlocked || audioError !== null || draftAuthorityBlocked,
  );

  function draftIntent(
    segmentId: string,
    baseRevision: number,
    text: string,
    original: string,
  ): RevisionBoundReviewDraftIntent {
    return text.trim() === original.trim()
      ? { kind: 'delete', segmentId, baseRevision }
      : { kind: 'save', segmentId, baseRevision, text };
  }

  function queueDraftWrite(
    segmentId: string,
    baseRevision: number,
    text: string,
    original: string,
  ): Promise<void> {
    return draftWrites.request(draftIntent(segmentId, baseRevision, text, original));
  }

  async function loadReviewDraft(seg: SpeechSegment, baseRevision: number, baseline: string) {
    const seq = ++draftLoadSeq;
    try {
      // A fast A -> B -> A navigation can return while A's outgoing draft write is still queued.
      // Reading before that chain settles can recover the older database value, overwrite the newer
      // session edit, and then debounce the stale text back over the durable draft. Serialize the read
      // behind this segment's exact pending chain; a failed write keeps authority blocked below.
      await draftWrites.flushSegment(seg.id);
      const draft = await api.getReviewDraftV1(seg.id);
      if (seq !== draftLoadSeq || current?.id !== seg.id) return;
      draftConflict = null;
      draftRecovered = false;
      draftSaveFailed = false;
      draftLoadError = null;
      if (!draft) {
        draftWrites.acknowledge({ kind: 'delete', segmentId: seg.id, baseRevision });
      } else if (draft.segmentId !== seg.id) {
        // A malformed or stale IPC response must never place another clip's human text in this editor.
        // Treat response identity exactly like commit identity: fail closed and require a fresh read.
        throw new Error($t('inbox.error.draftIdentityMismatch'));
      } else if (draft.baseRevision === baseRevision && editText === baseline) {
        // Session memory can be newer than the last durable debounce (for example after a very fast
        // navigation round-trip). Never replace non-server editor text with a different persisted
        // candidate. Keep both visible and require the reviewer to choose.
        if (baseline !== lastLoadedOriginal && draft.text !== baseline) {
          draftConflict = draft;
        } else {
          editCache.set(seg.id, draft.text);
          draftWrites.acknowledge({
            kind: 'save',
            segmentId: seg.id,
            baseRevision,
            text: draft.text,
          });
          editText = draft.text;
          draftRecovered = draft.text.trim() !== baseline.trim();
        }
      } else {
        // Never merge human text automatically. Server truth remains in the editor and the persisted
        // local draft is shown beside it for an explicit reviewer choice.
        draftConflict = draft;
      }
      draftReadyId = seg.id;
    } catch (error) {
      if (seq !== draftLoadSeq || current?.id !== seg.id) return;
      draftReadyId = null;
      draftLoadError = formatPublicErrorReference(error) ?? $t('errors.unknown');
      notifications.error($t('review.draftLoadFailed'), {
        cause: error,
        publicDetail: api.reviewErrorMessage(error, $t('review.draftLoadFailedHint')),
      });
    }
  }

  // Load the editable text + waveform whenever the current clip changes.
  $effect(() => {
    const seg = current;
    const revision = seg ? (reviewRevisions[seg.id] ?? null) : null;
    if (!seg || (seg.id === lastLoadedId && revision === lastLoadedRevision)) return;
    // Stash the OUTGOING clip's unsaved edit before we switch away — but if the user reverted it back
    // to the original, DROP any previously-cached edit so a discarded correction can't resurrect.
    if (lastLoadedId) {
      const outgoingDraftReadable = draftReadyId === lastLoadedId && draftLoadError === null;
      if (editText.trim() !== lastLoadedOriginal.trim()) {
        editCache.set(lastLoadedId, editText);
      } else {
        editCache.delete(lastLoadedId);
      }
      // A pending/failed read may hide an existing human draft. Never turn navigation into an
      // authority-free delete of that unseen row. An unresolved stale-revision conflict is already
      // durable; write only when the reviewer has also typed a distinct current-revision candidate.
      if (
        lastLoadedRevision !== null &&
        outgoingDraftReadable &&
        (!draftConflict || editText.trim() !== lastLoadedOriginal.trim())
      ) {
        void queueDraftWrite(lastLoadedId, lastLoadedRevision, editText, lastLoadedOriginal).catch(
          () => undefined,
        );
      }
    }
    draftReadyId = null;
    draftConflict = null;
    draftLoadError = null;
    draftRecovered = false;
    draftSaveFailed = false;
    lastLoadedId = seg.id;
    lastLoadedOriginal = originalText(seg);
    lastLoadedRevision = revision;
    editText = editCache.get(seg.id) ?? lastLoadedOriginal;
    currentTime = 0;
    playing = false;
    editingWordIndex = null; // never carry an open chip editor across clips
    editedChips = {}; // chip fixes are per-clip display state; don't leak them onto the next clip
    loadWaveform(seg);
    void ensureWordTimings(seg);
    void loadConsensus(seg);
    if (lastLoadedRevision !== null) void loadReviewDraft(seg, lastLoadedRevision, editText);
  });

  // Persist active edits after a short quiet period. Clip changes flush immediately above; the
  // backend revision-checks each write so a late autosave cannot resurrect a cleared old draft.
  $effect(() => {
    const seg = current;
    const text = editText;
    const original = lastLoadedOriginal;
    const baseRevision = lastLoadedRevision;
    if (!seg || draftReadyId !== seg.id || baseRevision === null) return;
    if (draftWrites.isDurable(draftIntent(seg.id, baseRevision, text, original))) return;
    const timer = window.setTimeout(() => {
      void queueDraftWrite(seg.id, baseRevision, text, original).catch(() => undefined);
    }, 500);
    return () => window.clearTimeout(timer);
  });

  async function flushActiveReviewDraft(): Promise<void> {
    if (lastLoadedId && (draftReadyId !== lastLoadedId || draftLoadError !== null)) {
      throw new Error($t('inbox.disabled.draftUnavailable'));
    }
    if (lastLoadedId && lastLoadedRevision !== null) {
      // A conflict row is already crash-safe. Do not delete it merely because the server baseline is
      // still visible; only a distinct new edit is safe to publish under the current revision.
      if (!draftConflict || editText.trim() !== lastLoadedOriginal.trim()) {
        await queueDraftWrite(lastLoadedId, lastLoadedRevision, editText, lastLoadedOriginal);
      }
    }
    await draftWrites.flushAll();
  }

  const unregisterDraftFlusher = registerReviewDraftFlusher(flushActiveReviewDraft);
  onDestroy(() => {
    // Native close awaits this same barrier from App before destruction. This fallback covers
    // ordinary component navigation; it cannot make teardown await, so errors remain visible through
    // the registered close path and the durable draft status already rendered in this workspace.
    void flushActiveReviewDraft().catch(() => undefined);
    unregisterDraftFlusher();
  });

  function useConflictingDraft() {
    if (!current || !draftConflict) return;
    editText = draftConflict.text;
    editCache.set(current.id, draftConflict.text);
    draftConflict = null;
    draftRecovered = true;
  }

  async function retryReviewDraftLoad() {
    const seg = current;
    const revision = seg ? reviewRevisions[seg.id] : undefined;
    if (!seg || !Number.isSafeInteger(revision) || (revision ?? -1) < 0) return;
    draftReadyId = null;
    draftLoadError = null;
    await loadReviewDraft(seg, revision as number, editText);
  }

  async function discardConflictingDraft() {
    const conflict = draftConflict;
    if (!current || !conflict) return;
    try {
      await api.deleteReviewDraftV1(current.id, conflict.baseRevision);
      if (current?.id === conflict.segmentId) {
        draftConflict = null;
        draftRecovered = false;
      }
    } catch (error) {
      notifications.error($t('review.draftDiscardFailed'), {
        cause: error,
        publicDetail: api.reviewErrorMessage(error, $t('review.draftDiscardFailed')),
      });
    }
  }

  // Drop a stale getWaveform response: switching clips A -> B while A's decode (up to ~30 s for a large
  // source) is still in flight must NOT let A's later-resolving waveform overwrite B's. Last-call-wins via
  // a monotonic sequence, mirroring segmentStore.load()'s loadSeq guard.
  let waveformLoadSeq = 0;
  async function loadWaveform(seg: SpeechSegment) {
    const seq = ++waveformLoadSeq;
    try {
      const data = await api.getWaveform(seg.audioPath, 240, seg.alignmentJson);
      if (seq !== waveformLoadSeq) return; // a newer clip started loading; this response is stale
      waveformData = data;
      waveformError = null;
    } catch (e) {
      if (seq !== waveformLoadSeq) return;
      // Audit 2026-08-05 #5 saw "the top waveform was blank during inspection". This catch used to
      // set [] and say nothing, so a FAILED decode rendered exactly like a silent clip: a reviewer
      // reads a flat strip as "this audio is quiet", not as "the app could not read your file". Same
      // failure-looking-like-success class the read guards in commands.ts exist for, and the reviewer
      // may then verify a clip they never actually saw the shape of.
      waveformData = [];
      waveformError = formatPublicErrorReference(e) ?? $t('errors.unknown');
      notifications.error($t('review.waveformFailed'), { cause: e });
    }
  }

  const dirty = $derived(current ? editText.trim() !== originalText(current).trim() : false);

  async function submit(acceptAsIs: boolean) {
    const seg = current;
    // Guard `retranscribing` too (retranscribe/markBad already do): a champion re-transcribe can take
    // several seconds, and accepting/saving mid-flight would record a human decision on the stale
    // pre-retranscribe draft that the in-flight run is about to overwrite — the human "accept" would
    // land on text the reviewer no longer sees.
    if (!seg || saving || retranscribing || aligning) return;
    if (draftAuthorityBlockedKey) {
      notifications.error($t(draftAuthorityBlockedKey));
      return;
    }
    if (eligibilityBlocked) {
      notifications.error(eligibilityReasonText(currentEligibility?.disabledReason));
      return;
    }
    if (acceptAsIs && dirty) {
      // Accept is only the unchanged fast path. Substituting `original` here used to discard the typed
      // correction, clear its durable draft, and certify text the reviewer had just changed.
      notifications.error($t('review.acceptDisabledEdited'));
      return;
    }
    // Same refusal as markBad: an unheard verdict is worse than no verdict in a VERBATIM corpus.
    // Same refusal as markBad: an unheard verdict is worse than no verdict in a VERBATIM corpus.
    if (audioError) {
      notifications.error($t('review.cannotDecideWithoutAudio'));
      return;
    }
    const original = originalText(seg).trim();
    const text = acceptAsIs ? original : editText.trim();
    // Never save an empty edit (mirrors the Save button's disabled guard — the Ctrl+Enter shortcut
    // would otherwise bypass it, blank the transcript, and split the segment's state).
    if (!acceptAsIs && !text) return;
    // Never let a placeholder ("[Pending WSL 7B ASR]" / "[ASR unavailable…]") be verified as gold: the
    // owner must re-transcribe it or mark it bad. Accepting it would count a non-transcript as reviewed
    // (inflating verified counts) while the export rubric silently drops it. An edit that REPLACES the
    // placeholder with real text is allowed (text is no longer a placeholder).
    if (isPlaceholderTranscript(text)) {
      notifications.error($t('review.cannotVerifyPlaceholder'));
      return;
    }
    saving = true;
    // Map to a real human decision: an actual change is an "edit" (the typed text becomes gold), a
    // no-change save is an "accept".
    const isEdit = !acceptAsIs && text !== original;
    const baseRevision = reviewRevisions[seg.id];
    if (!Number.isSafeInteger(baseRevision) || baseRevision < 0) {
      saving = false;
      notifications.error($t('notifications.loadSegmentsFailed'));
      void loadReviewPage(true);
      return;
    }
    let commitIntent: ReviewCommitIntent;
    try {
      // The typed decision is not a substitute for crash-safe draft durability. Persist the exact
      // visible editor first so a transport loss or process death cannot erase the correction while
      // the authoritative transaction is still unknown.
      await flushActiveReviewDraft();
      const visibleText = acceptAsIs ? originalText(current ?? seg).trim() : editText.trim();
      if (
        current?.id !== seg.id ||
        reviewRevisions[seg.id] !== baseRevision ||
        visibleText !== text ||
        (acceptAsIs && dirty) ||
        draftAuthorityBlocked
      ) {
        notifications.error($t('inbox.status.draftChangedDuringSave'));
        return;
      }
      // Accept-what-you-SEE: pass the displayed text even on accept so verdict_transcript (the
      // COALESCE-preferred gold source) becomes exactly what the human approved. Passing null let an
      // UNSEEN jury verdict_transcript survive and get exported as human-verified gold. For an edit the
      // typed text is the label; for an accept the displayed original is. (Backend only captures a
      // LOOP-0 memory / ledger row on 'edit', so an accept with text stays a pure verdict overwrite.)
      // Post the listening receipt BEFORE the verdict. The backend resolves this segment's
      // revision and canonical decoded-PCM content hash itself and refuses a verdict without sufficient evidence, so
      // a decision recorded on a clip nobody heard becomes impossible rather than merely discouraged.
      // A failed receipt finalization aborts this save while the draft, clip, focus and audio state
      // remain in place. Continuing with ambient or missing evidence would make the typed receipt
      // field decorative rather than an authorization boundary.
      const finalizedReceiptId = await finalizePlaybackAttempt(seg, baseRevision);
      if (!finalizedReceiptId) return;
      commitIntent = {
        segmentId: seg.id,
        baseRevision,
        decision: isEdit ? 'edit' : 'accept',
        transcript: text,
        reasonCode: null,
        playbackReceiptId: finalizedReceiptId,
      };
      const commit = await api.commitReviewV1({
        operationId: commitOperations.idFor(commitIntent),
        ...commitIntent,
      });
      const effectEventId = committedEffectId(seg.id, baseRevision, commit);
      if (effectEventId === null) return;
      commitOperations.resolve(commitIntent);
      playbackAttempts.resolve(seg.id, baseRevision);
      undoHistory = [
        ...undoHistory,
        { id: seg.id, effectEventId, operationId: crypto.randomUUID() },
      ];
      segments.update((list) =>
        list.map((stored) =>
          stored.id === seg.id
            ? {
                ...stored,
                verified: true,
                verdictTranscript: commit.authoritativeTranscript,
                annotatedTranscript: isEdit
                  ? commit.authoritativeTranscript
                  : stored.annotatedTranscript,
              }
            : stored,
        ),
      );
      // Capture navigation state BEFORE removing the saved row. Once it is filtered out, `current`
      // necessarily changes (or becomes null while the next lightweight row hydrates), which cannot
      // distinguish an ordinary successful advance from a real mid-flight user navigation.
      const visibleId = current?.id ?? null;
      if (visibleId === seg.id) {
        lastLoadedOriginal = text;
        editText = text;
        editedChips = {};
      }
      reviewRows = reviewRows.filter((s) => s.id !== seg.id);
      const remainingRevisions = { ...reviewRevisions };
      delete remainingRevisions[seg.id];
      reviewRevisions = remainingRevisions;
      const remainingEligibility = { ...reviewEligibility };
      delete remainingEligibility[seg.id];
      reviewEligibility = remainingEligibility;
      reviewTotal = Math.max(0, reviewTotal - 1);
      void refreshSegmentStats();
      editCache.delete(seg.id); // persisted — drop the in-progress copy
      draftWrites.acknowledge({ kind: 'delete', segmentId: seg.id, baseRevision });
      notifications.success($t('saved'));
      // If the reviewer really navigated during the slow decision call, keep that clip selected after
      // the removal shifted array indices and never copy seg's editor state into it.
      if (visibleId !== seg.id) {
        const visibleIndex = visibleId ? queue.findIndex((row) => row.id === visibleId) : -1;
        if (visibleIndex >= 0) index = visibleIndex;
        return;
      }
      advance();
    } catch (e) {
      if (api.isCommandErrorV1(e, 'NO_PLAYBACK_EVIDENCE'))
        notifications.error($t('review.mustListen'));
      else {
        notifications.error($t('notifications.saveFailed'));
        if (api.isCommandErrorV1(e, 'STALE_REVISION')) {
          void loadReviewPage(true);
        }
      }
    } finally {
      saving = false;
    }
  }

  function advance() {
    // After a save the just-verified clip drops to the done tail; jump to the next clip that still
    // needs a human (the first remaining pending). If NONE remain, set index = -1 so `current` resolves
    // to null and the "all done" empty state renders — never clamp back onto an already-verified clip
    // (which would silently re-open finished work for re-editing).
    const nextPending = queue.findIndex((s) => !s.verified);
    index = nextPending >= 0 ? nextPending : -1;
  }
  async function go(delta: number) {
    if (current && (draftReadyId !== current.id || draftLoadError !== null)) {
      notifications.error(
        $t(draftLoadError ? 'inbox.disabled.draftUnavailable' : 'inbox.disabled.draftLoading'),
      );
      return;
    }
    const target = Math.max(0, Math.min(queue.length - 1, index + delta));
    if (target === index) return;
    // Navigation never writes review truth. The selection effect below keeps the outgoing edit in the
    // session-local editCache and restores it when the reviewer returns; only submit() may durably
    // commit transcript text together with its immutable human-decision effect.
    const targetRow = queue[target];
    if (targetRow) {
      try {
        await hydrateReviewRow(targetRow.id);
      } catch (error) {
        notifications.error($t('notifications.loadSegmentsFailed'), {
          cause: error,
        });
        return;
      }
    }
    index = target;
  }
  function resetToOriginal() {
    if (current) editText = originalText(current);
    editedChips = {}; // revert the chip overlay too, so it can't claim a fix the reset gold lacks
  }

  // Transient word-bounded playback window. While set, the player plays exactly
  // [wordStartOverride, wordEndOverride] — so tapping a word plays THAT word (and Loop loops that
  // word, not the whole span, because BOTH bounds are overridden). Cleared whenever playback stops
  // (word finished, pause, clip switch) or on any non-word retarget (replay, waveform seek), so the
  // main Play button always plays the full spoken span.
  let wordStartOverride = $state<number | null>(null);
  let wordEndOverride = $state<number | null>(null);
  function clearWordOverride() {
    wordStartOverride = null;
    wordEndOverride = null;
  }
  $effect(() => {
    if (!playing) clearWordOverride();
  });

  // Which word chip is being edited in place (null = none). Reset on clip change.
  let editingWordIndex = $state<number | null>(null);
  // Display-only overlay of committed chip fixes (index → corrected word). Shows the reviewer's
  // correction on the strip WITHOUT mutating alignment_json — the gold lives in editText; the chip's
  // word text + timings stay the ASR alignment (a listening aid). Reset on clip change / Reset, so
  // Accept-as-is / undo never revert to a chip that claims a fix the gold transcript lacks.
  let editedChips = $state<Record<number, string>>({});
  const chipText = (w: WordTimestamp, i: number) => editedChips[i] ?? w.word;

  // Single tap / Enter / Space on a word chip = hear EXACTLY that word. Listen-only: it never opens
  // an input or steals keyboard focus, so the single-key review flow keeps working. Word times are
  // clip-relative; wordPlayBounds returns absolute file time, padded + clamped.
  function playWord(w: WordTimestamp) {
    const b = wordPlayBounds(w, range.startTime, range.endTime, SPOKEN_PAD);
    // Idempotent: a double-click dispatches click,click,dblclick — don't hard-reseek the same word
    // 2–3× (an audible stutter) when it is already the active playing window.
    if (playing && wordStartOverride === b.start && wordEndOverride === b.end) return;
    wordStartOverride = b.start;
    wordEndOverride = b.end;
    currentTime = b.start;
    playing = true;
  }

  // Double-tap / F2 on a chip = edit that word inline (and hear it). A DELIBERATE gesture, so a plain
  // listen tap never mounts an input under the reviewer's keystrokes.
  function startWordEdit(w: WordTimestamp, i: number) {
    playWord(w);
    editingWordIndex = i;
  }

  // Commit an inline chip edit into the working transcript (the gold). Empty or unchanged = cancel,
  // never a silent delete. replaceWordToken is STRICT: it rewrites only the confidently-located
  // token (position when the counts corroborate it, else a UNIQUE exact match), else returns null
  // and we fall back to selecting the word in the transcript editor — never guess-rewrite a repeated
  // Sorani word. editText is the only thing persisted; the chip fix is shown via editedChips, so
  // alignment_json never diverges from the gold.
  // Returns true when the caller should restore focus to the chip (clean commit or unchanged cancel);
  // false when it fell back to the transcript editor (editWord moved focus there — leave it).
  function commitWordEdit(i: number, w: WordTimestamp, value: string, viaBlur = false): boolean {
    if (editingWordIndex !== i) return false; // stale blur after Escape/commit already closed it
    editingWordIndex = null;
    const current = chipText(w, i);
    const fix = value.trim();
    if (!fix || fix === current) return true; // unchanged / empty = cancel, no rewrite
    const replaced = replaceWordToken(editText, i, current, fix, words.length);
    if (replaced === null) {
      // Ambiguous / diverged — can't place the fix confidently. On an explicit Enter, jump the human
      // into the transcript editor to place it; on a passive blur, just cancel (don't yank focus).
      if (!viaBlur) editWord(w, i);
      return false;
    }
    editText = replaced;
    editedChips = { ...editedChips, [i]: fix };
    return true;
  }

  function cancelWordEdit(i: number) {
    if (editingWordIndex === i) editingWordIndex = null;
  }

  // editWord: select THAT word in the transcript textarea (never rewrites it) — the safe fallback
  // when an inline edit can't be located confidently. Prefer the i-th editor token when it still
  // matches, else the first exact match, else just focus.
  function editWord(w: WordTimestamp, i: number) {
    if (!editEl) return;
    editEl.focus();
    const text = editText;
    const tokens: Array<{ start: number; len: number; word: string }> = [];
    const re = /\S+/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(text)) !== null)
      tokens.push({ start: m.index, len: m[0].length, word: m[0] });
    const wanted = chipText(w, i);
    const target =
      tokens[i] && tokens[i].word === wanted ? tokens[i] : tokens.find((t) => t.word === wanted);
    if (target) editEl.setSelectionRange(target.start, target.start + target.len);
  }
  function replay() {
    clearWordOverride(); // replay the WHOLE segment, not a stale tapped-word window
    currentTime = playStart;
    playing = true;
  }

  // 3-bin confidence → style class (research: discrete bins scan faster than a gradient).
  function confClass(c: number | undefined | null): string {
    if (c == null) return '';
    if (c < 0.6) return 'conf-low';
    if (c < 0.85) return 'conf-mid';
    return '';
  }

  // The transcript editor, so single-key shortcuts can focus it (`e`) and Escape can leave it.
  let editEl = $state<HTMLTextAreaElement | undefined>();

  // Keyboard-first review flow (parity with the Review Inbox): a=accept, e=edit, x=mark-bad,
  // space=play/pause, r=replay, n/→=next, p/←=prev, Ctrl/Cmd+Enter=save & next. Single-key actions
  // NEVER fire while typing in the transcript (or any input), and never hijack space/Enter from a
  // focused button/link — so nothing corrupts the edit text or breaks native control activation.
  function onKeydown(e: KeyboardEvent) {
    // The Review Inbox overlays this surface with its OWN window keydown handler. While it is open,
    // every keystroke belongs to the inbox — firing review-mode actions (accept/reject/undo/save) on
    // the HIDDEN clip behind it silently records human decisions the owner never made, corrupting gold
    // labels. Guard FIRST, before the Ctrl+Enter save, or an inbox edit's Ctrl+Enter also saves+verifies
    // the invisible ReviewMode clip.
    if (get(showReviewInbox)) return;
    // Save & next: works everywhere, including mid-edit.
    if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
      e.preventDefault();
      submit(false);
      return;
    }
    const el = e.target as HTMLElement | null;
    const typing =
      !!el &&
      (el.tagName === 'TEXTAREA' ||
        el.tagName === 'INPUT' ||
        el.tagName === 'SELECT' ||
        el.isContentEditable);
    if (typing) {
      // Escape drops focus so the single-key review shortcuts resume; otherwise let typing through.
      if (e.key === 'Escape') {
        e.preventDefault();
        editEl?.blur();
      }
      return;
    }
    // Bare single keys only — never steal a browser/OS chord (Ctrl+A, etc.).
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    // Let a focused button/link keep its native space/Enter activation.
    if ((el?.tagName === 'BUTTON' || el?.tagName === 'A') && (e.key === ' ' || e.key === 'Enter')) {
      return;
    }
    // Match on the PHYSICAL key (layout-independent): the owner types Sorani corrections with a
    // Central Kurdish layout active, where e.key is 'ا'/'ب'/… — every advertised letter shortcut
    // (A/E/X/R/N/P) went dead after Escape-ing the textarea until the OS layout was toggled back,
    // once per edited clip. physicalKey maps KeyA→'a' and falls back to e.key for Space/arrows/⌫.
    switch (physicalKey(e)) {
      case 'a':
        e.preventDefault();
        submit(true);
        break;
      case 'e':
        e.preventDefault();
        editEl?.focus();
        break;
      case 'x':
        e.preventDefault();
        void markBad();
        break;
      case ' ':
        e.preventDefault();
        playing = !playing;
        break;
      case 'r':
        e.preventDefault();
        replay();
        break;
      case 'n':
      case 'ArrowRight':
      case 'ArrowDown':
        e.preventDefault();
        void go(1);
        break;
      case 'p':
      case 'ArrowLeft':
      case 'ArrowUp':
        e.preventDefault();
        void go(-1);
        break;
      case 'Backspace':
        e.preventDefault();
        void undoLast();
        break;
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if reviewLoading && reviewInitialTotal === 0}
  <div class="flex min-h-full items-center [justify-content:safe_center] p-6" aria-busy="true">
    <div class="text-sm text-subtle">{$t('loading')}</div>
  </div>
{:else if reviewLoadError && !current}
  <div
    class="flex min-h-full items-center [justify-content:safe_center] p-6"
    data-testid="review-load-error"
    role="alert"
  >
    <EmptyState
      variant="error"
      title={$t('notifications.loadSegmentsFailed')}
      description={reviewLoadError}
    >
      <button type="button" class="btn btn-primary !text-sm" onclick={() => loadReviewPage(true)}>
        {$t('retry')}
      </button>
    </EmptyState>
  </div>
{:else if !current && reviewTotal > 0}
  <div class="flex min-h-full items-center [justify-content:safe_center] p-6" aria-busy="true">
    <div class="text-sm text-subtle">{$t('loading')}</div>
  </div>
{:else if !current}
  <div
    class="flex min-h-full items-center [justify-content:safe_center] p-6"
    data-testid="review-terminal"
  >
    <div class="flex flex-col items-center gap-4 text-center">
      <EmptyState
        variant="empty"
        title={$t('review.allDone')}
        description={searchScoped
          ? $t('review.searchScopeEmpty')
          : focusNarrowed
            ? $t('review.focusScopeEmpty')
            : $t('review.allDoneHint')}
      />
      <div class="flex flex-wrap justify-center gap-2">
        {#if progress.allReviewed && onExport}
          <button
            type="button"
            class="btn btn-primary !text-sm"
            data-testid="review-terminal-export"
            onclick={onExport}
          >
            {$t('review.exportDataset')}
          </button>
        {/if}
        {#if onDone}
          <button
            type="button"
            class="btn btn-secondary !text-sm"
            data-testid="review-terminal-done"
            onclick={onDone}
          >
            {$t('review.backToLibrary')}
          </button>
        {/if}
      </div>
    </div>
  </div>
{:else}
  {@const isVerified = current.verified}
  <div class="flex h-full min-h-0 flex-col">
    <div class="min-h-0 flex-1 overflow-y-auto" bind:this={reviewScroller}>
      <div class="review-stack mx-auto flex max-w-3xl flex-col gap-5 px-4 py-6">
        <!-- Completion banner: every clip verified → surface the next steps (export / done). The clips
           stay below so the reviewer can still scrub back and re-check any of them. -->
        {#if progress.allReviewed}
          <div
            class="review-wide card border border-emerald-700/40 bg-emerald-950/20 p-5 text-center"
            data-testid="review-complete"
          >
            <div class="text-lg font-semibold text-emerald-300">
              {$t('review.completeTitle').replace('{n}', String(reviewCorpusTotal))}
            </div>
            <p class="mt-1 text-sm text-subtle">{$t('review.completeHint')}</p>
            <div class="mt-4 flex flex-wrap justify-center gap-2">
              {#if onExport}
                <button
                  type="button"
                  class="btn btn-primary !text-sm"
                  data-testid="review-complete-export"
                  onclick={onExport}
                >
                  {$t('review.exportDataset')}
                </button>
              {/if}
              <button type="button" class="btn btn-secondary !text-sm" onclick={() => (index = 0)}>
                {$t('review.reviewAgain')}
              </button>
              {#if onDone}
                <button type="button" class="btn btn-secondary !text-sm" onclick={onDone}>
                  {$t('review.backToLibrary')}
                </button>
              {/if}
            </div>
          </div>
        {/if}

        {#if subsetScoped}
          <!-- Scope is never silent: the reviewer always sees they're on a subset. A voice focus is a
             subset too, and used to be the one that said nothing (review 2026-08-20). -->
          <div
            class="review-wide rounded-lg border border-amber-600/40 bg-amber-950/30 px-3 py-2 text-xs text-amber-300"
            data-testid="review-scope-banner"
          >
            {$t(searchScoped ? 'review.searchScope' : 'review.focusScope')
              .replace('{n}', String(queue.length))
              .replace('{m}', String(reviewCorpusTotal))}
          </div>
        {/if}

        <!-- Progress -->
        <div class="review-progress">
          <div class="flex items-center justify-between gap-3">
            <span class="text-sm font-medium text-muted">
              {$t('review.progress')
                .replace('{n}', String(index + 1))
                .replace('{total}', String(queue.length))}
            </span>
            <div class="flex items-center gap-2">
              <button
                type="button"
                data-testid="suspect-first-toggle"
                onclick={toggleSuspectFirst}
                title={$t('review.suspectFirstHint')}
                aria-pressed={suspectFirst}
                class="rounded-md border px-2 py-1 text-xs transition-colors {suspectFirst
                  ? 'border-accent bg-accent/15 text-accent'
                  : 'border-surface-3 text-subtle hover:text-muted'}"
              >
                {$t('review.suspectFirst')}
              </button>
              <span class="badge {isVerified ? 'badge-verified' : 'badge-pending'}">
                {isVerified ? $t('verified') : $t('pending')}
              </span>
            </div>
          </div>
          <div class="mt-2 h-1.5 overflow-hidden rounded-full bg-surface-3">
            <div
              class="h-full rounded-full bg-accent transition-all duration-300"
              style="width: {progress.percent}%"
            ></div>
          </div>

          <!-- WHY this clip needs a human, from the jury's own record (P1.2). These are the reason codes
             the T0/T1/T2 gates persisted at decision time, NOT a fresh inference from the row's current
             values — the audio badge elsewhere answers "what is true now", this answers "why was this
             decided". A clip escalated for low_snr whose audio was later replaced still says so here.
             Absent record renders NOTHING: most clips were never escalated, and rows decided before the
             codes existed have no record. "No reasons" would be a claim the data cannot support. -->
          {#if escalationReasons}
            <div class="mt-2 flex flex-wrap items-center gap-1.5" data-testid="escalation-reasons">
              <span class="text-[11px] uppercase tracking-wider text-subtle">
                {$t('reason.whyEscalated')}
              </span>
              {#each escalationReasons.reasonCodes as code (code)}
                {@const key = reasonLabelKey(code)}
                <span
                  class="reason-chip reason-{reasonTone(code)}"
                  title={escalationReasons.policyVersion ?? undefined}
                >
                  <!-- An unknown code renders VERBATIM rather than being dropped: if the backend gained a
                     code this build does not know, an untranslated string is a prompt to add it, while a
                     silently shorter list hides the new information entirely. -->
                  {key ? $t(key) : code}
                </span>
              {/each}
            </div>
          {/if}
          <div class="mt-1 flex items-center justify-between gap-3 text-xs text-subtle">
            <!-- True-10 audit: source-file orientation — in a hundreds-of-clips sitting the reviewer
               had no idea WHICH recording the current clip came from.
               2026-08-05: the chunk position is its own span with a NOUN. It used to render as a bare
               "61/144" glued to the filename, one line under "Clip 1 of 144" and opposite
               "67 of 144 reviewed" — three unlabelled fractions sharing a denominator by coincidence.
               dir="ltr" stays on the FILENAME only; the CKB noun must not be forced into an LTR run. -->
            <span class="flex min-w-0 items-center gap-1.5">
              <span
                class="truncate"
                dir="ltr"
                title={current.audioPath}
                data-testid="review-source-file"
              >
                {segmentSourceFilename(current.audioPath)}
              </span>
              {#if chunkLabel}
                <span class="shrink-0" data-testid="review-chunk-label"
                  >{$t('chunk')} <span dir="ltr">{chunkLabel}</span></span
                >
              {/if}
            </span>
            <span class="shrink-0">
              {$t('review.reviewedCount')
                .replace('{done}', String(progress.done))
                .replace('{total}', String(progress.total))}
            </span>
          </div>
        </div>

        <!-- THE AUDIO BLOCK: waveform + scope hint + transport, one card, immediately above the
           correction box. Owner feedback 2026-08-13 — these were three separate cards with the
           editor far below, so listening and correcting could not be done without scrolling. -->
        <div class="review-audio-card card overflow-hidden">
          <div class="overflow-hidden">
            {#if waveformError}
              <!-- A flat strip and a failed decode look identical, and the reviewer would read the flat
               strip as "quiet audio". Say which one it is, in place, and offer the retry. -->
              <div
                class="flex items-center justify-between gap-3 p-3 text-xs text-amber-300"
                data-testid="review-waveform-error"
                role="status"
              >
                <span class="min-w-0 truncate">{$t('review.waveformFailed')}</span>
                <button
                  type="button"
                  class="btn btn-secondary shrink-0 !text-xs"
                  onclick={() => current && loadWaveform(current)}
                >
                  {$t('retry')}
                </button>
              </div>
            {:else}
              <Waveform
                waveform={waveformData}
                currentTime={clipPosition}
                duration={clipLength}
                {playing}
                wordTimestamps={words}
                onSeek={(time) => {
                  clearWordOverride(); // a manual scrub leaves word-playback mode; play on to the span end
                  currentTime = range.startTime + time;
                }}
              />
            {/if}
          </div>

          <!-- Honest playback-scope hint: decision playback always covers the full segment. -->
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

          <!-- Audio player — decision playback is bounded to the complete database segment. A tapped
           word may temporarily narrow the play window, while evidence coordinates remain anchored to
           the full immutable source span. -->
          <!-- True-10 audit: honor the autoplay setting (it was hardcoded off here while honored in
           curate mode) — with it on, advancing to the next clip auto-plays the bounded spoken span,
           removing one keypress + wait per clip, hundreds of times per review sitting. -->
          <!-- start/endTime honour the transient tap-a-word override so a tapped word plays (and loops)
           exactly itself; otherwise the full spoken span. -->
          {#if !$showReviewInbox}
            {#key `${current.id}\0${String(reviewRevisions[current.id])}`}
              <AudioPlayer
                bind:this={reviewAudioPlayer}
                audioPath={current.audioPath}
                clipKey={current.id}
                startTime={wordStartOverride ?? playStart}
                endTime={wordEndOverride ?? playEnd}
                displayStart={playStart}
                displayEnd={playEnd}
                evidenceStart={range.startTime}
                evidenceEnd={range.endTime}
                bind:currentTime
                bind:duration={playerDuration}
                bind:playing
                bind:audioError
                bind:heardMs
                bind:playbackReceiptId
                bind:playbackMediaGrantId
                bind:playbackClipDurationMs
                bind:heardIntervals
                autoplay={$settings.autoplaySegments}
                requirePlaybackProof={true}
                expectedRevision={reviewRevisions[current.id]}
              />
            {/key}
          {/if}
          {#if audioError}
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
                    bind:value={technicalUnusableReason}
                    disabled={saving}
                    aria-describedby="review-unusable-help"
                  >
                    <option value="">{$t('review.unusable.reasonPlaceholder')}</option>
                    {#each technicalUnusableReasons as reason}
                      <option value={reason}>{$t(`review.unusable.reason.${reason}`)}</option>
                    {/each}
                  </select>
                </label>
                <button
                  type="button"
                  class="btn btn-secondary !text-amber-100"
                  onclick={() => void markTechnicallyUnusable()}
                  disabled={saving || !technicalUnusableReason || draftAuthorityBlocked || dirty}
                  aria-describedby={draftAuthorityBlocked
                    ? 'review-unusable-help review-draft-disabled-reason'
                    : dirty
                      ? 'review-unusable-help review-reject-disabled-reason'
                      : 'review-unusable-help'}
                >
                  {saving ? $t('review.unusable.marking') : $t('review.unusable.mark')}
                </button>
              </div>
            </fieldset>
          {/if}
        </div>

        <!-- Transcript: big, directly editable -->
        <div class="review-transcript-card card p-5">
          <div class="flex items-center justify-between gap-3">
            <div>
              <label
                for="review-transcript-editor"
                class="text-xs font-semibold uppercase tracking-wider text-muted"
              >
                {$t('transcript')}
              </label>
              <p class="mt-0.5 text-xs text-subtle">{$t('review.editHint')}</p>
              {#if draftModels.length > 0}
                <p class="mt-1 text-[11px] text-subtle" dir="ltr">
                  {$t('review.draftBy')}
                  <span class="font-medium text-muted"
                    >{draftModels.map((m) => api.engineLabel(m)).join(', ')}</span
                  >
                  {$t('review.notHumanVerified')}
                </p>
              {/if}
            </div>
            {#if dirty}
              <button
                type="button"
                class="ring-focus shrink-0 rounded-token px-2 py-1 text-xs text-subtle transition-colors hover:text-default"
                onclick={resetToOriginal}
              >
                {$t('review.reset')}
              </button>
            {/if}
          </div>
          <ReviewDraftRecovery
            conflict={draftConflict}
            serverText={originalText(current)}
            loadFailed={draftLoadError !== null}
            saving={draftSaving}
            saveFailed={draftSaveFailed}
            recovered={draftRecovered}
            onUseConflict={useConflictingDraft}
            onDiscardConflict={() => void discardConflictingDraft()}
            onRetryLoad={() => void retryReviewDraftLoad()}
          />
          <textarea
            id="review-transcript-editor"
            bind:value={editText}
            bind:this={editEl}
            dir="rtl"
            lang="ckb"
            spellcheck="false"
            class="review-transcript-input input font-kurdish mt-3 min-h-[150px] w-full resize-none text-2xl leading-loose"
            placeholder={$t('editTranscript')}
          ></textarea>
        </div>

        <!-- Listen-strip: tap a word to hear it; low-confidence words are highlighted -->
        {#if words.length > 0}
          <ReviewWordStrip
            {words}
            {activeWordIndex}
            bind:editingWordIndex
            {chipText}
            confidenceClass={confClass}
            isEdited={(wordIndex) => !!editedChips[wordIndex]}
            onReplay={replay}
            onPlay={playWord}
            onStartEdit={startWordEdit}
            onCommitEdit={commitWordEdit}
            onCancelEdit={cancelWordEdit}
          />
        {/if}

        <!-- Consensus draft: an offline best-of-N vote across this clip's ASR models. Contested words
           (the models disagreed) are highlighted so the eye lands on likely errors first; "Use draft"
           starts the edit from a transcript better than any single model. -->
        <!-- Fix-the-draft tools: the current transcript is wrong -> re-transcribe with the champion,
           or flag the clip bad (excluded from export, kept + reversible). -->
        <div class="review-secondary flex flex-wrap items-center gap-2">
          <span class="text-[11px] uppercase tracking-wider text-subtle"
            >{$t('review.retranscribe')}</span
          >
          <button
            type="button"
            class="btn btn-secondary !text-xs"
            onclick={retranscribe}
            disabled={retranscribing || saving}
            title={$t('review.retranscribeChampionTitle')}
          >
            {retranscribing ? $t('review.retranscribing') : $t('review.retranscribeChampion')}
          </button>
          {#if $settings.juryCloudOptIn}
            <button
              type="button"
              class="btn btn-secondary !text-xs"
              onclick={() => void runCloudCheck()}
              disabled={cloudChecking || retranscribing || saving}
              title={$t('review.cloudCheckTitle')}
            >
              {cloudChecking ? $t('review.cloudChecking') : $t('review.cloudCheck')}
            </button>
          {/if}
        </div>

        <!-- Cloud watcher verdict: Gemini's audio-grounded reading of THIS clip. Advisory only — the
           reviewer decides; "use this text" fills the editor and still requires Save & next. -->
        {#if cloudCheck && current && cloudCheck.id === current.id}
          {@const verdict = cloudCheck.result.verdict}
          <div
            class="review-secondary rounded-md border border-cortex-700/40 bg-cortex-900/40 p-3 space-y-2"
          >
            {#if verdict}
              {#if verdict.transcript.trim() === editText.trim()}
                <p class="text-xs text-emerald-300">
                  {$t('review.cloudCheckAgrees')} ({Math.round(verdict.confidence * 100)}%)
                </p>
              {:else}
                <p dir="rtl" lang="ckb" class="font-mono text-sm text-end">{verdict.transcript}</p>
                <p class="text-[11px] text-subtle">
                  {verdict.reason} · {Math.round(verdict.confidence * 100)}% · {verdict.votes}×
                </p>
                <button
                  type="button"
                  class="btn btn-secondary !text-xs"
                  onclick={() => (editText = verdict.transcript)}
                >
                  {$t('review.cloudCheckUse')}
                </button>
              {/if}
            {:else}
              <p class="text-xs text-amber-300">
                {$t('review.cloudCheckEscalated')}{cloudCheck.result.error
                  ? ` — ${cloudCheck.result.error}`
                  : ''}
              </p>
            {/if}
          </div>
        {/if}
      </div>
    </div>

    <ReviewActionBar
      {eligibilityBlocked}
      eligibilityReason={eligibilityReasonText(currentEligibility?.disabledReason)}
      audioUnavailable={audioError !== null}
      draftBlockedKey={draftAuthorityBlockedKey}
      {dirty}
      {saving}
      {retranscribing}
      previousDisabled={index === 0}
      undoAvailable={undoHistory.length > 0}
      {decisionBlocked}
      editHasText={editText.trim().length > 0}
      onPrevious={() => void go(-1)}
      onUndo={() => void undoLast()}
      onReject={() => void markBad()}
      onAccept={() => void submit(true)}
      onSave={() => void submit(false)}
    />
  </div>
{/if}

<style>
  /* Escalation reason chips. Tinted by SEVERITY, not by uniform alarm: `policy_hold` and
     `uncalibrated_bucket` are neutral because they describe the operator's setting and the evidence
     available, not a fault in the clip — colouring them like bad audio would tell the reviewer to
     distrust a recording nothing is wrong with. Colour is never the only cue: every chip carries its
     own words, so this reads correctly in monochrome and to a screen reader. */
  .reason-chip {
    display: inline-flex;
    align-items: center;
    border-radius: 9999px;
    padding: 1px 8px;
    font-size: 0.6875rem;
    line-height: 1.4;
    border: 1px solid transparent;
  }
  .reason-danger {
    background: color-mix(in srgb, var(--danger) 16%, transparent);
    border-color: color-mix(in srgb, var(--danger) 40%, transparent);
    color: var(--text);
  }
  .reason-warning {
    background: color-mix(in srgb, var(--warning) 16%, transparent);
    border-color: color-mix(in srgb, var(--warning) 40%, transparent);
    color: var(--text);
  }
  .reason-neutral {
    background: var(--surface-3);
    border-color: var(--border);
    color: var(--text-muted, var(--text));
  }

  /* On the supported 1000x600 workstation floor, playback and transcript share the evidence row.
     Secondary diagnostics stay available below it in the scroller, while the core correction loop is
     completely visible without page scrolling. Taller and narrower screens retain the linear flow. */
  @media (min-width: 800px) and (max-height: 700px) {
    .review-stack {
      display: grid;
      max-width: none;
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
      align-content: start;
      gap: 0.5rem;
      padding: 0.5rem 1rem;
    }
    .review-wide,
    .review-progress,
    .review-secondary {
      grid-column: 1 / -1;
    }
    .review-transcript-card {
      padding: 0.75rem;
    }
    .review-transcript-input {
      min-height: 100px;
      font-size: 1.125rem;
      line-height: 1.75rem;
    }
  }
</style>
