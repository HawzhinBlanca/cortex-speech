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
import { formatPublicErrorReference } from './errorText';
import { t, type TranslationKey } from './i18n';
import {
  ReviewCommitOperationLedger,
  reviewCommitFailureDisposition,
  type ReviewCommitIntent,
} from './reviewCommitOperation';
import { isCommittedReviewFor } from './reviewCommitResult';
import type { ReviewInboxDraftController } from './reviewInboxDraft.svelte';
import type { ReviewInboxQueueController } from './reviewInboxQueue.svelte';
import type { ReviewPlaybackController } from './reviewModePlayback.svelte';
import { segments } from './stores/segmentStore';
import type { SpeechSegment } from './types';

type HumanDecision = 'accept' | 'edit' | 'reject';
const GENERIC_REVIEW_FLAG_RATIONALE = 'Flagged for second-pass adjudication';
type ReviewFlagIntent = {
  operationId: string;
  segmentId: string;
  baseRevision: number;
  rationale: string;
};

interface DecisionDependencies {
  queue: ReviewInboxQueueController;
  draft: ReviewInboxDraftController;
  playback: ReviewPlaybackController;
  setStatus: (message: string) => void;
}

export interface InboxActionKeys {
  draft: TranslationKey | null;
  accept: TranslationKey | null;
  edit: TranslationKey | null;
  reject: TranslationKey | null;
  skip: TranslationKey | null;
  flag: TranslationKey | null;
  saveEdit: TranslationKey | null;
  undo: TranslationKey | null;
}

export function createReviewInboxDecisionController(
  deps: DecisionDependencies,
  durableUndo: DurableReviewUndoController = sharedDurableReviewUndo,
) {
  const state = $state({
    submitting: false,
    technicalReason: '' as api.TechnicalUnusableReasonV1 | '',
    technicalIntent: null as api.MarkSegmentUnusableRequestV1 | null,
    flagIntent: null as ReviewFlagIntent | null,
  });
  const operations = new ReviewCommitOperationLedger();
  const disposeUndoProjection = durableUndo.registerProjectionConsumer({
    reloadProjection: deps.queue.reloadProjection,
    projectionReceipt: deps.queue.projectionReceipt,
  });

  const current = deps.queue.current;
  const revision = deps.queue.currentRevision;
  const publicError = (error: unknown) =>
    formatPublicErrorReference(error) ?? get(t)('inbox.error.unknown');

  const hasActiveGenericFlag = (segment: SpeechSegment | null) =>
    !!segment &&
    segment.escalated &&
    segment.verdict === 'escalated' &&
    segment.rationale === GENERIC_REVIEW_FLAG_RATIONALE;

  function resetSelection() {
    state.technicalReason = '';
    state.technicalIntent = null;
    state.flagIntent = null;
  }

  function resetSession() {
    resetSelection();
    void durableUndo.refresh();
  }

  function playbackError(error: unknown) {
    if (!error || typeof error !== 'object')
      return error instanceof Error && error.message.includes('E_NO_PLAYBACK_EVIDENCE');
    const value = error as { code?: unknown; message?: unknown };
    return (
      value.code === 'PLAYBACK_RECEIPT_REQUIRED' ||
      value.code === 'INVALID_PLAYBACK_RECEIPT' ||
      value.code === 'NO_PLAYBACK_EVIDENCE' ||
      value.code === 'E_NO_PLAYBACK_EVIDENCE' ||
      (typeof value.message === 'string' && value.message.includes('E_NO_PLAYBACK_EVIDENCE'))
    );
  }

  function failure(key: TranslationKey, error: unknown) {
    return playbackError(error)
      ? get(t)('review.mustListen')
      : get(t)(key, { err: publicError(error) });
  }

  function requireRevision(segment: SpeechSegment): number | null {
    const baseRevision = deps.queue.state.revisions[segment.id];
    const eligibility = deps.queue.state.eligibility[segment.id];
    if (!eligibility?.eligible || !Number.isSafeInteger(baseRevision) || baseRevision < 0) {
      deps.setStatus(get(t)('inbox.disabled.notEligible'));
      return null;
    }
    return baseRevision;
  }

  function committedEffectId(
    segment: SpeechSegment,
    baseRevision: number,
    commit: Awaited<ReturnType<typeof api.commitReviewV1>>,
  ) {
    if (!isCommittedReviewFor(commit, segment.id, baseRevision)) {
      deps.setStatus(
        get(t)('inbox.status.loadFailed', { err: get(t)('inbox.error.commitIdentityMismatch') }),
      );
      return null;
    }
    const effectId = api.reviewEffectId(commit.decisionId);
    if (effectId === null) {
      deps.setStatus(
        get(t)('inbox.status.loadFailed', { err: get(t)('inbox.error.commitIdentityMismatch') }),
      );
      return null;
    }
    return effectId;
  }

  async function settleTruthProjection(truthLease: string, expected?: ExpectedDesktopReviewUndo) {
    if (!durableUndo.invalidateForNewAction(truthLease)) return false;
    const settled = await durableUndo.reconcileTruthProjections(truthLease, segments, expected);
    if (!settled) {
      deps.setStatus(
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
    deps.setStatus(get(t)('review.truthWriteUncertainRestart'));
  }

  async function commitHuman(decision: HumanDecision) {
    const segment = current();
    if (
      !segment ||
      state.submitting ||
      segment.humanDecision ||
      deps.playback.state.audioError ||
      deps.draft.blockedKey() ||
      (decision !== 'edit' && deps.draft.state.editing)
    )
      return;
    const truthKey = newTruthDisabledKey();
    if (truthKey) {
      deps.setStatus(get(t)(truthKey));
      return;
    }
    const draft = deps.draft.state;
    if (decision === 'edit' && (!draft.editText.trim() || draft.editingForId !== segment.id)) {
      if (draft.editingForId !== segment.id) deps.setStatus(get(t)('inbox.disabled.staleEdit'));
      return;
    }
    const baseRevision = requireRevision(segment);
    if (baseRevision === null) return;
    const transcript =
      decision === 'reject' ? null : decision === 'edit' ? draft.editText.trim() : draft.baseline;
    const truthLease = durableUndo.beginTruthWrite();
    if (!truthLease) {
      deps.setStatus(get(t)('inbox.disabled.saving'));
      return;
    }
    state.submitting = true;
    let intent: ReviewCommitIntent | null = null;
    let writerInvoked = false;
    try {
      if (decision === 'edit') {
        await deps.draft.flush();
        if (
          current()?.id !== segment.id ||
          revision() !== baseRevision ||
          draft.editText.trim() !== transcript
        ) {
          deps.setStatus(get(t)('inbox.status.draftChangedDuringSave'));
          return;
        }
      }
      const receiptId = await deps.playback.finalize(segment, baseRevision);
      if (!receiptId) return;
      const visibleTranscript =
        decision === 'reject' ? null : decision === 'edit' ? draft.editText.trim() : draft.baseline;
      if (
        current()?.id !== segment.id ||
        revision() !== baseRevision ||
        visibleTranscript !== transcript ||
        deps.draft.blockedKey() ||
        (decision === 'edit' && (!draft.editing || draft.editingForId !== segment.id))
      ) {
        deps.setStatus(get(t)('inbox.status.draftChangedDuringSave'));
        return;
      }
      intent = {
        segmentId: segment.id,
        baseRevision,
        decision,
        transcript,
        reasonCode: null,
        playbackReceiptId: receiptId,
      };
      if (!durableUndo.truthWriteStillCurrent(truthLease)) {
        deps.setStatus(get(t)('inbox.disabled.saving'));
        return;
      }
      writerInvoked = true;
      const decisionOperationId = operations.idFor(intent);
      const commit = await api.commitReviewV1({ operationId: decisionOperationId, ...intent });
      const effectId = committedEffectId(segment, baseRevision, commit);
      if (effectId === null) {
        stopForAmbiguousWrite(truthLease, {
          schema: 1,
          code: 'INVALID_COMMIT_RESPONSE',
          retryable: false,
        });
        return;
      }
      operations.resolve(intent);
      deps.playback.resolve(segment.id, baseRevision);
      deps.draft.finishCommit(segment.id, baseRevision);
      if (
        !(await settleTruthProjection(truthLease, {
          kind: 'decision',
          effectEventId: effectId,
          segmentId: segment.id,
          decision,
          sourceOperationId: decisionOperationId,
        }))
      )
        return;
      const status = {
        accept: 'inbox.status.accepted',
        edit: 'inbox.status.edited',
        reject: 'inbox.status.rejected',
      } as const;
      deps.setStatus(get(t)(status[decision]));
    } catch (error) {
      if (intent) {
        const disposition = reviewCommitFailureDisposition(error);
        if (disposition !== 'retain-exact-retry') operations.resolve(intent);
        if (disposition === 'restart-playback') {
          deps.playback.restartAfterProvenNonCommit(segment.id, baseRevision);
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
      const key = {
        accept: 'inbox.status.acceptFailed',
        edit: 'inbox.status.editFailed',
        reject: 'inbox.status.rejectFailed',
      } as const;
      deps.setStatus(failure(key[decision], error));
      if (api.isCommandErrorV1(error, 'STALE_REVISION')) void deps.queue.reloadProjection();
    } finally {
      state.submitting = false;
      durableUndo.endTruthWrite(truthLease);
    }
  }

  async function markTechnicallyUnusable() {
    const segment = current();
    const reason = state.technicalReason;
    if (
      !segment ||
      !deps.playback.state.audioError ||
      !reason ||
      state.submitting ||
      deps.draft.state.editing ||
      deps.draft.blockedKey()
    )
      return;
    const truthKey = newTruthDisabledKey();
    if (truthKey) {
      deps.setStatus(get(t)(truthKey));
      return;
    }
    const baseRevision = deps.queue.state.revisions[segment.id];
    if (!Number.isSafeInteger(baseRevision) || baseRevision < 0) {
      deps.setStatus(get(t)('review.unusable.authorityMissing'));
      return;
    }
    const request =
      state.technicalIntent?.segmentId === segment.id &&
      state.technicalIntent.baseRevision === baseRevision &&
      state.technicalIntent.reason === reason
        ? state.technicalIntent
        : { operationId: crypto.randomUUID(), segmentId: segment.id, baseRevision, reason };
    state.technicalIntent = request;
    const truthLease = durableUndo.beginTruthWrite();
    if (!truthLease) {
      deps.setStatus(get(t)('inbox.disabled.saving'));
      return;
    }
    state.submitting = true;
    let writerInvoked = false;
    try {
      await deps.draft.flush();
      if (
        current()?.id !== segment.id ||
        deps.queue.state.revisions[segment.id] !== baseRevision ||
        state.technicalReason !== reason ||
        deps.draft.blockedKey() ||
        deps.draft.state.editing
      ) {
        deps.setStatus(get(t)('inbox.status.draftChangedDuringSave'));
        return;
      }
      if (!durableUndo.truthWriteStillCurrent(truthLease)) {
        deps.setStatus(get(t)('inbox.disabled.saving'));
        return;
      }
      writerInvoked = true;
      const response = await api.markSegmentUnusableV1(request);
      if (
        response.segmentId !== request.segmentId ||
        response.committedRevision !== request.baseRevision + 1 ||
        response.reason !== request.reason ||
        !/^flag-effect:[1-9][0-9]*$/.test(response.effectId)
      ) {
        stopForAmbiguousWrite(truthLease, {
          schema: 1,
          code: 'INVALID_UNUSABLE_RESPONSE',
          retryable: false,
        });
        deps.setStatus(get(t)('review.unusable.invalidResponse'));
        return;
      }
      state.technicalIntent = null;
      const effectEventId = Number.parseInt(response.effectId.slice('flag-effect:'.length), 10);
      if (!Number.isSafeInteger(effectEventId) || effectEventId <= 0) {
        stopForAmbiguousWrite(truthLease, {
          schema: 1,
          code: 'INVALID_UNUSABLE_RESPONSE',
          retryable: false,
        });
        deps.setStatus(get(t)('review.unusable.invalidResponse'));
        return;
      }
      if (
        !(await settleTruthProjection(truthLease, {
          kind: 'flag',
          effectEventId,
          segmentId: segment.id,
          sourceOperationId: request.operationId,
          flagKind: { kind: 'technicalUnusable', reason: request.reason },
        }))
      )
        return;
      state.technicalReason = '';
      deps.setStatus(get(t)('review.unusable.marked'));
    } catch (error) {
      if (writerInvoked && ambiguousWriteFailure(error)) {
        stopForAmbiguousWrite(truthLease, error);
        return;
      }
      deps.setStatus(
        get(t)('review.unusable.markFailedWithError', {
          err: api.reviewErrorMessage(error, get(t)('review.unusable.markFailedHint')),
        }),
      );
      if (
        api.isCommandErrorV1(error, 'STALE_REVISION') ||
        api.isCommandErrorV1(error, 'HUMAN_TRUTH_ALREADY_COMMITTED') ||
        api.isCommandErrorV1(error, 'SEGMENT_NOT_FOUND')
      ) {
        // The backend proved this operation did not commit. Retire the revision-bound identity and
        // reconcile before another click so a surviving row cannot reuse obsolete authority/UUID.
        state.technicalIntent = null;
        await deps.queue.reloadProjection();
      }
    } finally {
      state.submitting = false;
      durableUndo.endTruthWrite(truthLease);
    }
  }

  async function skip() {
    if (!current() || state.submitting || durableUndo.blocksSurfaceTransition()) return;
    // This is deliberately navigation, not review truth. Keep the historical internal name only as
    // a shortcut compatibility alias; the visible contract says exactly that no decision is saved.
    deps.setStatus(get(t)('inbox.status.skipped'));
    await deps.queue.advance();
  }

  async function flag() {
    const segment = current();
    if (hasActiveGenericFlag(segment)) {
      deps.setStatus(get(t)('inbox.disabled.alreadyFlagged'));
      return;
    }
    if (
      !segment ||
      deps.draft.state.editing ||
      state.submitting ||
      segment.humanDecision ||
      deps.draft.blockedKey()
    )
      return;
    const truthKey = newTruthDisabledKey();
    if (truthKey) {
      deps.setStatus(get(t)(truthKey));
      return;
    }
    const baseRevision = requireRevision(segment);
    if (baseRevision === null) return;
    const rationale = GENERIC_REVIEW_FLAG_RATIONALE;
    const request =
      state.flagIntent?.segmentId === segment.id &&
      state.flagIntent.baseRevision === baseRevision &&
      state.flagIntent.rationale === rationale
        ? state.flagIntent
        : { operationId: crypto.randomUUID(), segmentId: segment.id, baseRevision, rationale };
    state.flagIntent = request;
    const truthLease = durableUndo.beginTruthWrite();
    if (!truthLease) {
      deps.setStatus(get(t)('inbox.disabled.saving'));
      return;
    }
    state.submitting = true;
    let writerInvoked = false;
    try {
      if (!durableUndo.truthWriteStillCurrent(truthLease)) {
        deps.setStatus(get(t)('inbox.disabled.saving'));
        return;
      }
      writerInvoked = true;
      const commit = await api.recordReviewFlag(request);
      if (
        commit.segmentId !== segment.id ||
        commit.segment.id !== segment.id ||
        !Number.isSafeInteger(commit.effectEventId) ||
        commit.effectEventId <= 0 ||
        commit.priorRevision !== baseRevision ||
        commit.flagRevision !== baseRevision + 1
      ) {
        deps.setStatus(get(t)('inbox.status.flagInvalidResponse'));
        stopForAmbiguousWrite(truthLease, {
          schema: 1,
          code: 'INVALID_FLAG_RESPONSE',
          retryable: false,
        });
        return;
      }
      state.flagIntent = null;
      if (
        !(await settleTruthProjection(truthLease, {
          kind: 'flag',
          effectEventId: commit.effectEventId,
          segmentId: segment.id,
          sourceOperationId: request.operationId,
          flagKind: { kind: 'generic' },
        }))
      )
        return;
      deps.setStatus(get(t)('inbox.status.flagged'));
    } catch (error) {
      if (writerInvoked && ambiguousWriteFailure(error)) {
        stopForAmbiguousWrite(truthLease, error);
        return;
      }
      deps.setStatus(get(t)('inbox.status.flagFailed', { err: publicError(error) }));
      if (api.isCommandErrorV1(error, 'STALE_REVISION')) {
        state.flagIntent = null;
        await deps.queue.reloadProjection();
      }
    } finally {
      state.submitting = false;
      durableUndo.endTruthWrite(truthLease);
    }
  }

  async function undo() {
    if (state.submitting || deps.draft.state.editing) return;
    if (durableUndo.state.truthWriteAmbiguous) {
      deps.setStatus(get(t)('review.truthWriteUncertainRestart'));
      return;
    }
    if (durableUndo.state.truthWriteInFlight) {
      deps.setStatus(get(t)('inbox.disabled.saving'));
      return;
    }
    const blockedKey =
      durableUndo.state.target?.kind === 'decision' ? deps.draft.blockedKey() : null;
    if (blockedKey) {
      deps.setStatus(get(t)(blockedKey));
      return;
    }
    state.submitting = true;
    let undoCrossedIpcBoundary = false;
    try {
      if (durableUndo.state.truthProjectionPending) {
        const recovered = await durableUndo.retryTruthProjections(segments);
        deps.setStatus(
          get(t)(
            recovered ? 'review.truthProjectionRecovered' : 'review.truthProjectionReloadRequired',
          ),
        );
        return;
      }
      if (durableUndo.state.status === 'failed') {
        await durableUndo.refresh();
        if (durableUndo.state.status === 'failed') {
          deps.setStatus(get(t)('review.undoStatusRetryFailed'));
        }
        return;
      }
      if (durableUndo.state.status === 'projectionStale') {
        await reconcileUndoProjection();
        return;
      }

      const actionRequest = durableUndo.beginRequest();
      if (!actionRequest) return;
      // Flag Undo never changes transcript truth or draft rows. Drain an actual in-memory edit, but
      // do not make a safely retained stale/load-failed draft an obstacle to reversing the flag.
      if (actionRequest.target.kind === 'decision' || deps.draft.state.pending) {
        await deps.draft.flush();
      }
      if (
        actionRequest.target.kind === 'decision' &&
        (deps.draft.blockedKey() || deps.draft.state.editing)
      ) {
        durableUndo.releaseUnsent(actionRequest);
        deps.setStatus(get(t)('inbox.status.draftChangedDuringSave'));
        return;
      }
      let rawOutcome: unknown;
      try {
        // Once invocation begins, any later exception is a lost/invalid response until the exact
        // operation identity is reconciled against backend truth.
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
          deps.setStatus(get(t)('review.undoUncertain'));
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
        deps.setStatus(get(t)('review.undoUncertain'));
        return;
      }

      durableUndo.requireProjectionReload(actionRequest, outcome);
      await reconcileUndoProjection();
    } catch (error) {
      const request = durableUndo.state.inFlight
        ? currentUndoRequest(durableUndo.state.target, durableUndo.state.operationId)
        : null;
      if (request) {
        if (undoCrossedIpcBoundary) durableUndo.markAmbiguous(request, error);
        else durableUndo.releaseUnsent(request);
      }
      deps.setStatus(get(t)('inbox.status.undoFailed', { err: publicError(error) }));
    } finally {
      state.submitting = false;
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
      deps.setStatus(get(t)('review.undoProjectionReloadRequired'));
      return;
    }
    if (durableUndo.state.status === 'failed') {
      deps.setStatus(get(t)('review.undoAppliedAuthorityUnavailable'));
      return;
    }
    if (pending.outcome === 'conflict') {
      deps.setStatus(
        get(t)('inbox.status.undoFailed', {
          err: get(t)('inbox.error.undoDecisionConflict'),
        }),
      );
      return;
    }
    deps.setStatus(
      get(t)(
        deps.queue.state.rows.some((row) => row.id === pending.target.segmentId)
          ? 'inbox.status.undone'
          : 'inbox.status.undoRestoredOutsideScope',
      ),
    );
  }

  function undoAuthorityKey(): TranslationKey | null {
    if (durableUndo.state.truthWriteAmbiguous) return 'review.truthWriteUncertainRestart';
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

  function editMutationBlocked(): boolean {
    return state.submitting || durableUndo.blocksSurfaceTransition();
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

  function actionKeys(): InboxActionKeys {
    const segment = current();
    const draft = deps.draft.state;
    const draftKey = deps.draft.blockedKey();
    const eligibility = deps.queue.currentEligibility();
    const baseRevision = revision();
    const truthKey = newTruthDisabledKey();
    const authorityUnavailable =
      !!segment &&
      (!eligibility?.eligible || !Number.isSafeInteger(baseRevision) || (baseRevision ?? -1) < 0);
    const shared: TranslationKey | null = state.submitting
      ? 'inbox.disabled.saving'
      : truthKey
        ? truthKey
        : segment?.humanDecision
          ? 'inbox.disabled.alreadyReviewed'
          : authorityUnavailable
            ? 'inbox.disabled.notEligible'
            : draftKey
              ? draftKey
              : deps.playback.state.audioError
                ? 'inbox.disabled.audioUnavailable'
                : null;
    const edit = draft.editing
      ? 'inbox.disabled.editInProgress'
      : state.submitting
        ? 'inbox.disabled.saving'
        : truthKey
          ? truthKey
          : draft.loadError
            ? 'inbox.disabled.draftUnavailable'
            : draft.readyId !== segment?.id
              ? 'inbox.disabled.draftLoading'
              : draft.conflict
                ? 'inbox.disabled.draftConflict'
                : segment?.humanDecision
                  ? 'inbox.disabled.alreadyReviewed'
                  : authorityUnavailable
                    ? 'inbox.disabled.notEligible'
                    : null;
    return {
      draft: draftKey,
      accept: draft.editing ? 'review.acceptDisabledEdited' : shared,
      edit,
      reject: draft.editing ? 'inbox.disabled.editInProgress' : shared,
      skip: state.submitting
        ? 'inbox.disabled.saving'
        : truthKey
          ? truthKey
          : !deps.queue.canAdvance()
            ? 'inbox.disabled.noNext'
            : null,
      flag: draft.editing
        ? 'inbox.disabled.editInProgress'
        : state.submitting
          ? 'inbox.disabled.saving'
          : truthKey
            ? truthKey
            : draftKey
              ? draftKey
              : segment?.humanDecision
                ? 'inbox.disabled.alreadyReviewed'
                : hasActiveGenericFlag(segment)
                  ? 'inbox.disabled.alreadyFlagged'
                  : null,
      saveEdit: shared
        ? shared
        : !draft.editText.trim()
          ? 'inbox.disabled.emptyEdit'
          : draft.editingForId !== segment?.id
            ? 'inbox.disabled.staleEdit'
            : null,
      // Undo owns a distinct recovery state machine. A confirmed write whose projection reload
      // failed deliberately leaves `status=failed` + `truthProjectionPending=true`; routing Undo
      // through the new-truth gate made the only Reconcile action impossible to invoke.
      undo:
        durableUndo.state.target?.kind === 'decision' && draft.editing
          ? 'inbox.disabled.editInProgress'
          : state.submitting || durableUndo.state.truthWriteInFlight
            ? 'inbox.disabled.saving'
            : durableUndo.state.truthWriteAmbiguous
              ? 'review.truthWriteUncertainRestart'
              : durableUndo.state.target?.kind === 'decision' && draftKey
                ? draftKey
                : undoAuthorityKey(),
    };
  }

  return {
    state,
    publicError,
    resetSelection,
    resetSession,
    actionKeys,
    accept: () => commitHuman('accept'),
    commitEdit: () => commitHuman('edit'),
    reject: () => commitHuman('reject'),
    markTechnicallyUnusable,
    skip,
    flag,
    undo,
    newTruthDisabledKey,
    editMutationBlocked,
    undoActionKey,
    undoErrorCode: () => durableUndo.state.errorCode,
    refreshUndo: durableUndo.refresh,
    disposeUndoProjection,
  };
}

export function safeInboxEvidence(value: string | null | undefined) {
  try {
    return JSON.stringify(JSON.parse(value ?? '[]'), null, 2);
  } catch {
    return value ?? '';
  }
}

export type ReviewInboxDecisionController = ReturnType<typeof createReviewInboxDecisionController>;
