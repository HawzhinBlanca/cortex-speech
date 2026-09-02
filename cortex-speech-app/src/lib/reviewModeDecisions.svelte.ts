import { get } from 'svelte/store';
import * as api from './commands';
import {
  sharedDurableReviewUndo,
  validatedDesktopReviewUndoOutcome,
  withReviewOperationTimeout,
  type DurableReviewUndoController,
  type ExactDesktopReviewUndoRequest,
  type ExpectedDesktopReviewUndo,
} from './durableReviewUndo.svelte';
import { t, type TranslationKey } from './i18n';
import {
  ReviewCommitOperationLedger,
  reviewCommitFailureDisposition,
  type ReviewCommitIntent,
} from './reviewCommitOperation';
import { isCommittedReviewFor } from './reviewCommitResult';
import type { ReviewModeDraftController } from './reviewModeDraft.svelte';
import type { ReviewModePlaybackController } from './reviewModePlayback.svelte';
import type { ReviewModeQueueController } from './reviewModeQueue.svelte';
import { isPlaceholderTranscript } from './segmentQuality';
import { segments } from './stores/segmentStore';
import { notifications } from './stores/notificationStore';
import type { SpeechSegment } from './types';

interface DecisionDependencies {
  queue: ReviewModeQueueController;
  draft: ReviewModeDraftController;
  playback: ReviewModePlaybackController;
  editText: () => string;
  setEditText: (text: string) => void;
  originalText: (segment: SpeechSegment) => string;
  dirty: () => boolean;
  retranscribing: () => boolean;
  aligning: () => boolean;
  resetWords: () => void;
}

export function createReviewModeDecisionController(
  deps: DecisionDependencies,
  durableUndo: DurableReviewUndoController = sharedDurableReviewUndo,
) {
  const state = $state({
    saving: false,
    technicalUnusableReason: '' as api.TechnicalUnusableReasonV1 | '',
    technicalUnusableForId: null as string | null,
    technicalUnusableIntent: null as api.MarkSegmentUnusableRequestV1 | null,
  });
  const commitOperations = new ReviewCommitOperationLedger();
  let navigationSequence = 0;
  const disposeUndoProjection = durableUndo.registerProjectionConsumer({
    reloadProjection: async () => {
      const pendingFlagUndo = durableUndo.pendingProjection()?.target.kind === 'flag';
      if (!pendingFlagUndo) {
        try {
          await deps.draft.flush();
        } catch {
          return null;
        }
        if (draftBlocked()) return null;
      }
      // An exact flag inverse owns only verdict/rationale/escalation state. A retained
      // prior-revision draft must remain visible, but cannot block truth reconciliation.
      return deps.queue.reloadProjection();
    },
    projectionReceipt: deps.queue.projectionReceipt,
  });

  $effect(() => {
    const segmentId = deps.queue.current()?.id ?? null;
    if (segmentId === state.technicalUnusableForId) return;
    state.technicalUnusableForId = segmentId;
    state.technicalUnusableReason = '';
    state.technicalUnusableIntent = null;
  });

  const draftBlockedKey = (): TranslationKey | null => deps.draft.blockedKey();
  const draftBlocked = () => draftBlockedKey() !== null;
  const eligibilityBlocked = () => deps.queue.currentEligibility()?.eligible !== true;

  function eligibilityReasonText(reason: string | null | undefined): string {
    return reason === 'TRANSCRIPT_NOT_READY'
      ? get(t)('review.transcriptNotReady')
      : get(t)('review.eligibilityUnavailable');
  }

  function baseRevision(segment: SpeechSegment): number | null {
    const revision = deps.queue.state.revisions[segment.id];
    if (Number.isSafeInteger(revision) && revision >= 0) return revision;
    notifications.error(get(t)('notifications.loadSegmentsFailed'));
    void deps.queue.load(true);
    return null;
  }

  function committedEffectId(
    segment: SpeechSegment,
    revision: number,
    commit: Awaited<ReturnType<typeof api.commitReviewV1>>,
  ): number | null {
    // A malformed success is an ambiguous writer outcome, not authority to start a speculative
    // queue read. The caller hard-stops the truth lease and retains the exact visible intent.
    if (!isCommittedReviewFor(commit, segment.id, revision)) return null;
    const effectId = api.reviewEffectId(commit.decisionId);
    if (effectId === null) return null;
    return effectId;
  }

  function advance() {
    const next = deps.queue.queue().findIndex((segment) => !segment.verified);
    deps.queue.state.index = next >= 0 ? next : -1;
  }

  async function settleTruthProjection(truthLease: string, expected?: ExpectedDesktopReviewUndo) {
    if (!durableUndo.invalidateForNewAction(truthLease)) return false;
    const settled = await durableUndo.reconcileTruthProjections(truthLease, segments, expected);
    if (!settled) {
      notifications.error(
        get(t)(
          durableUndo.state.truthProjectionPending
            ? 'review.truthProjectionReloadRequired'
            : 'review.undoAppliedAuthorityUnavailable',
        ),
      );
    }
    return settled;
  }

  function ambiguousWriteFailure(error: unknown): boolean {
    return !api.isCommandErrorV1(error) || error.code === 'COMMIT_OUTCOME_UNKNOWN';
  }

  function stopForAmbiguousWrite(truthLease: string, error?: unknown) {
    durableUndo.markTruthWriteAmbiguous(truthLease, error);
    notifications.error(get(t)('review.truthWriteUncertainRestart'), { cause: error });
  }

  async function markBad() {
    const segment = deps.queue.current();
    if (!segment || state.saving || deps.retranscribing() || deps.aligning()) return;
    const truthKey = newTruthDisabledKey();
    if (truthKey) {
      notifications.error(get(t)(truthKey));
      return;
    }
    const blockedKey = draftBlockedKey();
    if (blockedKey) {
      notifications.error(get(t)(blockedKey));
      return;
    }
    if (deps.dirty()) {
      notifications.error(get(t)('review.rejectDisabledEdited'));
      return;
    }
    const eligibility = deps.queue.currentEligibility();
    if (eligibilityBlocked()) {
      notifications.error(eligibilityReasonText(eligibility?.disabledReason));
      return;
    }
    if (deps.playback.state.audioError) {
      notifications.error(get(t)('review.cannotDecideWithoutAudio'));
      return;
    }
    const revision = baseRevision(segment);
    if (revision === null) return;
    const truthLease = durableUndo.beginTruthWrite();
    if (!truthLease) {
      notifications.error(get(t)('inbox.disabled.saving'));
      return;
    }
    state.saving = true;
    let intent: ReviewCommitIntent | null = null;
    let writerInvoked = false;
    try {
      await deps.draft.flush();
      if (
        deps.queue.current()?.id !== segment.id ||
        deps.queue.state.revisions[segment.id] !== revision ||
        draftBlocked() ||
        deps.dirty()
      ) {
        notifications.error(get(t)('inbox.status.draftChangedDuringSave'));
        return;
      }
      const receiptId = await deps.playback.finalize(segment, revision);
      if (!receiptId) return;
      if (
        deps.queue.current()?.id !== segment.id ||
        deps.queue.state.revisions[segment.id] !== revision ||
        draftBlocked() ||
        deps.dirty()
      ) {
        notifications.error(get(t)('inbox.status.draftChangedDuringSave'));
        return;
      }
      intent = {
        segmentId: segment.id,
        baseRevision: revision,
        decision: 'reject',
        transcript: null,
        reasonCode: null,
        playbackReceiptId: receiptId,
      };
      if (!durableUndo.truthWriteStillCurrent(truthLease)) {
        notifications.error(get(t)('inbox.disabled.saving'));
        return;
      }
      writerInvoked = true;
      const decisionOperationId = commitOperations.idFor(intent);
      const commit = await api.commitReviewV1({
        operationId: decisionOperationId,
        ...intent,
      });
      const effectId = committedEffectId(segment, revision, commit);
      if (effectId === null) {
        stopForAmbiguousWrite(truthLease, {
          schema: 1,
          code: 'INVALID_COMMIT_RESPONSE',
          retryable: false,
        });
        return;
      }
      commitOperations.resolve(intent);
      deps.playback.resolve(segment.id, revision);
      deps.draft.acknowledgeCommitted(segment.id, revision);
      if (
        !(await settleTruthProjection(truthLease, {
          kind: 'decision',
          effectEventId: effectId,
          segmentId: segment.id,
          decision: 'reject',
          sourceOperationId: decisionOperationId,
        }))
      ) {
        return;
      }
      notifications.success(get(t)('review.markedBad'));
    } catch (error) {
      if (intent) {
        const disposition = reviewCommitFailureDisposition(error);
        if (disposition !== 'retain-exact-retry') commitOperations.resolve(intent);
        if (disposition === 'restart-playback') {
          deps.playback.restartAfterProvenNonCommit(segment.id, revision);
        }
        if (disposition === 'retain-exact-retry') {
          stopForAmbiguousWrite(truthLease, error);
          return;
        }
      }
      if (writerInvoked && ambiguousWriteFailure(error)) {
        stopForAmbiguousWrite(truthLease, error);
        return;
      }
      if (api.isCommandErrorV1(error, 'NO_PLAYBACK_EVIDENCE'))
        notifications.error(get(t)('review.mustListen'));
      else {
        notifications.error(get(t)('notifications.saveFailed'));
        if (api.isCommandErrorV1(error, 'STALE_REVISION')) await deps.queue.reloadProjection();
      }
    } finally {
      state.saving = false;
      durableUndo.endTruthWrite(truthLease);
    }
  }

  function validUnusableResponse(
    response: api.MarkedSegmentUnusableV1,
    request: api.MarkSegmentUnusableRequestV1,
  ) {
    return (
      response.segmentId === request.segmentId &&
      response.committedRevision === request.baseRevision + 1 &&
      response.reason === request.reason &&
      /^flag-effect:[1-9][0-9]*$/.test(response.effectId)
    );
  }

  async function markTechnicallyUnusable() {
    const segment = deps.queue.current();
    const reason = state.technicalUnusableReason;
    if (
      !segment ||
      !deps.playback.state.audioError ||
      !reason ||
      state.saving ||
      deps.retranscribing() ||
      deps.aligning()
    )
      return;
    const truthKey = newTruthDisabledKey();
    if (truthKey) {
      notifications.error(get(t)(truthKey));
      return;
    }
    const blockedKey = draftBlockedKey();
    if (blockedKey) {
      notifications.error(get(t)(blockedKey));
      return;
    }
    if (deps.dirty()) {
      notifications.error(get(t)('review.rejectDisabledEdited'));
      return;
    }
    const revision = baseRevision(segment);
    if (revision === null) return;
    const matching =
      state.technicalUnusableIntent?.segmentId === segment.id &&
      state.technicalUnusableIntent.baseRevision === revision &&
      state.technicalUnusableIntent.reason === reason
        ? state.technicalUnusableIntent
        : null;
    const request = matching ?? {
      operationId: crypto.randomUUID(),
      segmentId: segment.id,
      baseRevision: revision,
      reason,
    };
    state.technicalUnusableIntent = request;
    const truthLease = durableUndo.beginTruthWrite();
    if (!truthLease) {
      notifications.error(get(t)('inbox.disabled.saving'));
      return;
    }
    state.saving = true;
    let writerInvoked = false;
    try {
      await deps.draft.flush();
      if (
        deps.queue.current()?.id !== segment.id ||
        deps.queue.state.revisions[segment.id] !== revision ||
        state.technicalUnusableReason !== reason ||
        draftBlocked() ||
        deps.dirty()
      ) {
        notifications.error(get(t)('inbox.status.draftChangedDuringSave'));
        return;
      }
      if (!durableUndo.truthWriteStillCurrent(truthLease)) {
        notifications.error(get(t)('inbox.disabled.saving'));
        return;
      }
      writerInvoked = true;
      const response = await api.markSegmentUnusableV1(request);
      if (!validUnusableResponse(response, request)) {
        stopForAmbiguousWrite(truthLease, {
          schema: 1,
          code: 'INVALID_UNUSABLE_RESPONSE',
          retryable: false,
        });
        notifications.error(get(t)('review.unusable.invalidResponse'));
        return;
      }
      state.technicalUnusableIntent = null;
      const flagEffectId = Number.parseInt(response.effectId.slice('flag-effect:'.length), 10);
      if (!Number.isSafeInteger(flagEffectId) || flagEffectId <= 0) {
        stopForAmbiguousWrite(truthLease, {
          schema: 1,
          code: 'INVALID_UNUSABLE_RESPONSE',
          retryable: false,
        });
        notifications.error(get(t)('review.unusable.invalidResponse'));
        return;
      }
      if (
        !(await settleTruthProjection(truthLease, {
          kind: 'flag',
          effectEventId: flagEffectId,
          segmentId: segment.id,
          sourceOperationId: request.operationId,
          flagKind: { kind: 'technicalUnusable', reason: request.reason },
        }))
      )
        return;
      notifications.success(get(t)('review.unusable.marked'));
    } catch (error) {
      if (writerInvoked && ambiguousWriteFailure(error)) {
        stopForAmbiguousWrite(truthLease, error);
        return;
      }
      notifications.error(get(t)('review.unusable.markFailed'), {
        cause: error,
        publicDetail: api.reviewErrorMessage(error, get(t)('review.unusable.markFailedHint')),
      });
      if (
        api.isCommandErrorV1(error, 'STALE_REVISION') ||
        api.isCommandErrorV1(error, 'HUMAN_TRUTH_ALREADY_COMMITTED') ||
        api.isCommandErrorV1(error, 'SEGMENT_NOT_FOUND')
      ) {
        // These typed authority refusals are definitive non-commits: discard only that obsolete
        // operation identity, then hydrate authoritative truth before allowing a fresh attempt.
        state.technicalUnusableIntent = null;
        await deps.queue.reloadProjection();
      }
    } finally {
      state.saving = false;
      durableUndo.endTruthWrite(truthLease);
    }
  }

  async function undoLast() {
    if (state.saving || deps.retranscribing() || deps.aligning()) return;
    const blockedKey = durableUndo.state.target?.kind === 'decision' ? draftBlockedKey() : null;
    if (blockedKey) {
      notifications.error(get(t)(blockedKey));
      return;
    }
    if (durableUndo.state.target?.kind === 'decision' && deps.dirty()) {
      notifications.error(get(t)('review.undoDisabledEdited'));
      return;
    }

    state.saving = true;
    let undoCrossedIpcBoundary = false;
    try {
      if (durableUndo.state.truthProjectionPending) {
        const recovered = await durableUndo.retryTruthProjections(segments);
        if (recovered) notifications.success(get(t)('review.truthProjectionRecovered'));
        else notifications.error(get(t)('review.truthProjectionReloadRequired'));
        return;
      }
      if (durableUndo.state.status === 'failed') {
        await durableUndo.refresh();
        if (durableUndo.state.status === 'failed') {
          notifications.error(get(t)('review.undoStatusRetryFailed'));
        }
        return;
      }
      if (durableUndo.state.status === 'projectionStale') {
        await reconcileUndoProjection();
        return;
      }

      const actionRequest = durableUndo.beginRequest();
      if (!actionRequest) return;
      // Flag Undo owns verdict/rationale/escalation only. Persist a real in-memory correction, but
      // do not require an already-retained stale or unreadable draft to become readable first.
      if (actionRequest.target.kind === 'decision' || deps.dirty()) {
        await deps.draft.flush();
      }
      if (actionRequest.target.kind === 'decision' && (draftBlocked() || deps.dirty())) {
        durableUndo.releaseUnsent(actionRequest);
        notifications.error(get(t)('inbox.status.draftChangedDuringSave'));
        return;
      }
      let rawOutcome: unknown;
      try {
        // From this point onward an exception cannot prove that the backend did not commit. The
        // exact request must remain blocked/replayable until durable truth is reconciled.
        undoCrossedIpcBoundary = true;
        rawOutcome = await withReviewOperationTimeout(
          api.undoDesktopReviewActionV1(actionRequest.target, actionRequest.operationId),
          'E_DESKTOP_UNDO_TIMEOUT',
        );
      } catch (error) {
        if (api.isCommandErrorV1(error, 'STALE_UNDO_TARGET')) {
          durableUndo.requireProjectionReload(actionRequest, 'conflict');
          await reconcileUndoProjection();
        } else {
          durableUndo.markAmbiguous(actionRequest, error);
          notifications.error(get(t)('review.undoUncertain'), { cause: error });
        }
        return;
      }

      const outcome = validatedDesktopReviewUndoOutcome(rawOutcome, actionRequest.target);
      if (!outcome) {
        durableUndo.markAmbiguous(actionRequest, {
          schema: 1,
          code: 'INVALID_UNDO_RESPONSE',
          retryable: true,
        });
        notifications.error(get(t)('review.undoUncertain'));
        return;
      }

      durableUndo.requireProjectionReload(actionRequest, outcome);
      if (outcome !== 'conflict' && actionRequest.target.kind === 'decision') {
        deps.draft.forget(actionRequest.target.segmentId);
      }
      await reconcileUndoProjection();
    } catch (error) {
      const request = durableUndo.state.inFlight
        ? currentUndoRequest(durableUndo.state.target, durableUndo.state.operationId)
        : null;
      if (request) {
        if (undoCrossedIpcBoundary) durableUndo.markAmbiguous(request, error);
        else durableUndo.releaseUnsent(request);
      }
      notifications.error(get(t)('review.undoFailed'), { cause: error });
    } finally {
      state.saving = false;
    }
  }

  function currentUndoRequest(
    target: api.DesktopReviewUndoTargetV1 | null,
    operationId: string | null,
  ): ExactDesktopReviewUndoRequest | null {
    return target && operationId ? { target, operationId } : null;
  }

  async function reconcileUndoProjection() {
    const pending = durableUndo.pendingProjection();
    if (!pending) return;
    const projectionsApplied = await durableUndo.reconcileProjections(segments);
    if (!projectionsApplied) {
      notifications.error(get(t)('review.undoProjectionReloadRequired'));
      return;
    }
    if (durableUndo.state.status === 'failed') {
      notifications.error(get(t)('review.undoAppliedAuthorityUnavailable'));
      return;
    }
    if (pending.outcome === 'conflict') {
      notifications.error(get(t)('review.undoFailed'), {
        publicDetail: get(t)('inbox.error.undoDecisionConflict'),
      });
      return;
    }
    notifications.success(
      get(t)(
        deps.queue.state.rows.some((row) => row.id === pending.target.segmentId)
          ? 'review.undone'
          : 'review.undoRestoredOutsideScope',
      ),
    );
  }

  async function submit(acceptAsIs: boolean) {
    const segment = deps.queue.current();
    if (!segment || state.saving || deps.retranscribing() || deps.aligning()) return;
    const truthKey = newTruthDisabledKey();
    if (truthKey) {
      notifications.error(get(t)(truthKey));
      return;
    }
    const blockedKey = draftBlockedKey();
    if (blockedKey) {
      notifications.error(get(t)(blockedKey));
      return;
    }
    const eligibility = deps.queue.currentEligibility();
    if (eligibilityBlocked()) {
      notifications.error(eligibilityReasonText(eligibility?.disabledReason));
      return;
    }
    if (acceptAsIs && deps.dirty()) {
      notifications.error(get(t)('review.acceptDisabledEdited'));
      return;
    }
    if (deps.playback.state.audioError) {
      notifications.error(get(t)('review.cannotDecideWithoutAudio'));
      return;
    }
    const original = deps.originalText(segment).trim();
    const text = acceptAsIs ? original : deps.editText().trim();
    if ((!acceptAsIs && !text) || isPlaceholderTranscript(text)) {
      if (isPlaceholderTranscript(text))
        notifications.error(get(t)('review.cannotVerifyPlaceholder'));
      return;
    }
    const edit = !acceptAsIs && text !== original;
    const revision = baseRevision(segment);
    if (revision === null) return;
    const truthLease = durableUndo.beginTruthWrite();
    if (!truthLease) {
      notifications.error(get(t)('inbox.disabled.saving'));
      return;
    }
    state.saving = true;
    let intent: ReviewCommitIntent | null = null;
    let writerInvoked = false;
    try {
      await deps.draft.flush();
      const visibleText = acceptAsIs
        ? deps.originalText(deps.queue.current() ?? segment).trim()
        : deps.editText().trim();
      if (
        deps.queue.current()?.id !== segment.id ||
        deps.queue.state.revisions[segment.id] !== revision ||
        visibleText !== text ||
        (acceptAsIs && deps.dirty()) ||
        draftBlocked()
      ) {
        notifications.error(get(t)('inbox.status.draftChangedDuringSave'));
        return;
      }
      const receiptId = await deps.playback.finalize(segment, revision);
      if (!receiptId) return;
      const postPlaybackText = acceptAsIs
        ? deps.originalText(deps.queue.current() ?? segment).trim()
        : deps.editText().trim();
      if (
        deps.queue.current()?.id !== segment.id ||
        deps.queue.state.revisions[segment.id] !== revision ||
        postPlaybackText !== text ||
        (acceptAsIs && deps.dirty()) ||
        draftBlocked()
      ) {
        notifications.error(get(t)('inbox.status.draftChangedDuringSave'));
        return;
      }
      intent = {
        segmentId: segment.id,
        baseRevision: revision,
        decision: edit ? 'edit' : 'accept',
        transcript: text,
        reasonCode: null,
        playbackReceiptId: receiptId,
      };
      if (!durableUndo.truthWriteStillCurrent(truthLease)) {
        notifications.error(get(t)('inbox.disabled.saving'));
        return;
      }
      writerInvoked = true;
      const decisionOperationId = commitOperations.idFor(intent);
      const commit = await api.commitReviewV1({
        operationId: decisionOperationId,
        ...intent,
      });
      const effectId = committedEffectId(segment, revision, commit);
      if (effectId === null) {
        stopForAmbiguousWrite(truthLease, {
          schema: 1,
          code: 'INVALID_COMMIT_RESPONSE',
          retryable: false,
        });
        return;
      }
      commitOperations.resolve(intent);
      deps.playback.resolve(segment.id, revision);
      deps.draft.acknowledgeCommitted(segment.id, revision);
      if (
        !(await settleTruthProjection(truthLease, {
          kind: 'decision',
          effectEventId: effectId,
          segmentId: segment.id,
          decision: edit ? 'edit' : 'accept',
          sourceOperationId: decisionOperationId,
        }))
      )
        return;
      notifications.success(get(t)('saved'));
    } catch (error) {
      if (intent) {
        const disposition = reviewCommitFailureDisposition(error);
        if (disposition !== 'retain-exact-retry') commitOperations.resolve(intent);
        if (disposition === 'restart-playback') {
          deps.playback.restartAfterProvenNonCommit(segment.id, revision);
        }
        if (disposition === 'retain-exact-retry') {
          stopForAmbiguousWrite(truthLease, error);
          return;
        }
      }
      if (writerInvoked && ambiguousWriteFailure(error)) {
        stopForAmbiguousWrite(truthLease, error);
        return;
      }
      if (api.isCommandErrorV1(error, 'NO_PLAYBACK_EVIDENCE'))
        notifications.error(get(t)('review.mustListen'));
      else {
        notifications.error(get(t)('notifications.saveFailed'));
        if (api.isCommandErrorV1(error, 'STALE_REVISION')) await deps.queue.reloadProjection();
      }
    } finally {
      state.saving = false;
      durableUndo.endTruthWrite(truthLease);
    }
  }

  async function go(delta: number) {
    const blockedKey = newTruthDisabledKey();
    if (editMutationBlocked()) {
      notifications.error(get(t)(blockedKey ?? 'inbox.disabled.saving'));
      return;
    }
    const current = deps.queue.current();
    if (
      current &&
      (deps.draft.state.readyId !== current.id || deps.draft.state.loadError !== null)
    ) {
      notifications.error(
        get(t)(
          deps.draft.state.loadError
            ? 'inbox.disabled.draftUnavailable'
            : 'inbox.disabled.draftLoading',
        ),
      );
      return;
    }
    const queue = deps.queue.queue();
    const target = Math.max(0, Math.min(queue.length - 1, deps.queue.state.index + delta));
    const row = queue[target];
    const sequence = ++navigationSequence;
    if (target === deps.queue.state.index || !row) return;
    const targetId = row.id;
    try {
      const receipt = await deps.queue.hydrate(targetId);
      if (receipt === null || sequence !== navigationSequence || editMutationBlocked()) return;
      const resolvedIndex = deps.queue.queue().findIndex((candidate) => candidate.id === targetId);
      if (resolvedIndex < 0 || deps.queue.current()?.id === targetId) return;
      // No await can interleave between the final authority check and this selection write.
      deps.queue.state.index = resolvedIndex;
    } catch (error) {
      if (sequence === navigationSequence && !editMutationBlocked()) {
        notifications.error(get(t)('notifications.loadSegmentsFailed'), { cause: error });
      }
    }
  }

  function undoDisabledKey(): TranslationKey | null {
    if (state.saving) return 'inbox.disabled.saving';
    if (durableUndo.state.truthWriteAmbiguous) return 'review.truthWriteUncertainRestart';
    if (deps.retranscribing() || deps.aligning()) return 'review.undoDisabled.processing';
    if (durableUndo.state.target?.kind === 'decision' && deps.dirty())
      return 'review.undoDisabledEdited';
    const blockedKey = durableUndo.state.target?.kind === 'decision' ? draftBlockedKey() : null;
    if (blockedKey) return blockedKey;
    switch (durableUndo.state.status) {
      case 'ready':
        return null;
      case 'loading':
        return 'review.undoDisabled.loading';
      case 'failed':
        return null;
      case 'blocked':
        return blockedUndoKey();
      case 'none':
        return 'review.undoDisabled.none';
      case 'reconciling':
        return durableUndo.state.inFlight ? 'review.undoDisabled.reconciling' : null;
      case 'projectionStale':
        return null;
    }
  }

  function blockedUndoKey(): TranslationKey {
    switch (durableUndo.state.blockedReason) {
      case 'legacyHistory':
        return 'review.undoDisabled.legacyHistory';
      case 'latestDecisionUndone':
        return 'review.undoDisabled.latestDecisionUndone';
      case 'latestFlagUndone':
        return 'review.undoDisabled.latestFlagUndone';
      case 'decisionShadowed':
        return 'review.undoDisabled.decisionShadowed';
      case 'flagShadowed':
        return 'review.undoDisabled.flagShadowed';
      default:
        return 'review.undoDisabled.blocked';
    }
  }

  function newTruthDisabledKey(): TranslationKey | null {
    if (durableUndo.state.truthWriteAmbiguous) return 'review.truthWriteUncertainRestart';
    if (durableUndo.state.truthWriteInFlight) return 'inbox.disabled.saving';
    switch (durableUndo.state.status) {
      case 'loading':
        return 'review.undoDisabled.loading';
      case 'failed':
        return 'review.undoDisabled.failed';
      case 'reconciling':
        return 'review.undoDisabled.reconciling';
      case 'projectionStale':
        return 'review.undoDisabled.projectionStale';
      case 'ready':
      case 'none':
      case 'blocked':
        return null;
    }
  }

  /**
   * Freeze every renderer-side transcript mutation while a durable truth operation owns the
   * process-wide authority or while its projections still need reconciliation. This is intentionally
   * stronger than the action-bar `saving` flag: a failed projection and an ambiguous writer outcome
   * must retain the exact pre-click draft until restart/reconciliation establishes server truth.
   */
  function editMutationBlocked(): boolean {
    return state.saving || durableUndo.blocksSurfaceTransition();
  }

  function undoActionKey(): TranslationKey {
    if (durableUndo.state.truthProjectionPending) return 'review.reconcileSavedDecision';
    switch (durableUndo.state.status) {
      case 'failed':
        return 'review.retryUndoStatus';
      case 'reconciling':
        return 'review.retryExactUndo';
      case 'projectionStale':
        return 'review.reloadAfterUndo';
      default:
        return 'review.undoLast';
    }
  }

  return {
    state,
    eligibilityBlocked,
    eligibilityReasonText,
    draftBlockedKey,
    draftBlocked,
    submit,
    markBad,
    markTechnicallyUnusable,
    undoLast,
    undoDisabledKey,
    undoActionKey,
    undoErrorCode: () => durableUndo.state.errorCode,
    newTruthDisabledKey,
    editMutationBlocked,
    refreshUndo: durableUndo.refresh,
    disposeUndoProjection,
    advance,
    go,
  };
}

export type ReviewModeDecisionController = ReturnType<typeof createReviewModeDecisionController>;
