import { get } from 'svelte/store';
import * as api from './commands';
import { formatPublicErrorReference } from './errorText';
import { t, type TranslationKey } from './i18n';
import {
  ReviewDraftWriteCoordinator,
  type RevisionBoundReviewDraftIntent,
} from './reviewDraftWriteCoordinator';
import { notifications } from './stores/notificationStore';
import { showConfirmDialog } from './stores/uiStore';
import type { SpeechSegment } from './types';
import type { ReviewDraftV1 } from './commands';

interface DraftDependencies {
  current: () => SpeechSegment | null;
  editText: () => string;
  setEditText: (text: string) => void;
  originalText: (segment: SpeechSegment) => string;
  onSelectionActivated: (segment: SpeechSegment) => void;
}

export interface ReviewModeDraftState {
  lastLoadedId: string | null;
  lastOriginal: string;
  lastRevision: number | null;
  readyId: string | null;
  conflict: ReviewDraftV1 | null;
  loadError: string | null;
  recovered: boolean;
  saving: boolean;
  saveFailed: boolean;
}

export function createReviewModeDraftController(deps: DraftDependencies) {
  const state = $state<ReviewModeDraftState>({
    lastLoadedId: null,
    lastOriginal: '',
    lastRevision: null,
    readyId: null,
    conflict: null,
    loadError: null,
    recovered: false,
    saving: false,
    saveFailed: false,
  });
  const editCache = new Map<string, string>();
  let loadSequence = 0;
  let scheduledWriteTimer: number | null = null;
  let scheduledWriteGeneration = 0;
  const writes = new ReviewDraftWriteCoordinator({
    save: api.saveReviewDraftV1,
    delete: api.deleteReviewDraftV1,
    onStateChange: (segmentId) => {
      if (deps.current()?.id === segmentId) state.saving = writes.isWriting(segmentId);
    },
    onWriteSucceeded: (intent) => {
      if (deps.current()?.id === intent.segmentId && !writes.hasDesired(intent.segmentId)) {
        state.saveFailed = false;
      }
    },
    onWriteFailed: (intent, error) => {
      if (deps.current()?.id !== intent.segmentId) return;
      state.saveFailed = true;
      notifications.error(get(t)('review.draftSaveFailed'), {
        cause: error,
        publicDetail: api.reviewErrorMessage(error, get(t)('review.draftSaveFailedHint')),
      });
    },
  });

  function blockedKey(): TranslationKey | null {
    const current = deps.current();
    if (!current) return null;
    if (state.loadError) return 'inbox.disabled.draftUnavailable';
    if (state.readyId !== current.id) return 'inbox.disabled.draftLoading';
    if (state.conflict) return 'inbox.disabled.draftConflict';
    return null;
  }

  function intent(
    segmentId: string,
    baseRevision: number,
    text: string,
    original: string,
  ): RevisionBoundReviewDraftIntent {
    return text.trim() === original.trim()
      ? { kind: 'delete', segmentId, baseRevision }
      : { kind: 'save', segmentId, baseRevision, text };
  }

  function queueWrite(
    segmentId: string,
    baseRevision: number,
    text: string,
    original: string,
  ): Promise<void> {
    return writes.request(intent(segmentId, baseRevision, text, original));
  }

  function cancelScheduledWrite() {
    ++scheduledWriteGeneration;
    if (scheduledWriteTimer !== null) {
      window.clearTimeout(scheduledWriteTimer);
      scheduledWriteTimer = null;
    }
  }

  async function load(segment: SpeechSegment, baseRevision: number, baseline: string) {
    const sequence = ++loadSequence;
    try {
      await writes.flushSegment(segment.id);
      const draft = await api.getReviewDraftV1(segment.id);
      if (sequence !== loadSequence || deps.current()?.id !== segment.id) return;
      state.conflict = null;
      state.recovered = false;
      state.saveFailed = false;
      state.loadError = null;
      if (!draft) {
        writes.acknowledge({ kind: 'delete', segmentId: segment.id, baseRevision });
      } else if (draft.segmentId !== segment.id) {
        throw new Error(get(t)('inbox.error.draftIdentityMismatch'));
      } else if (draft.baseRevision === baseRevision && deps.editText() === baseline) {
        if (baseline !== state.lastOriginal && draft.text !== baseline) {
          state.conflict = draft;
        } else {
          editCache.set(segment.id, draft.text);
          writes.acknowledge({
            kind: 'save',
            segmentId: segment.id,
            baseRevision,
            text: draft.text,
          });
          deps.setEditText(draft.text);
          state.recovered = draft.text.trim() !== baseline.trim();
        }
      } else {
        state.conflict = draft;
      }
      state.readyId = segment.id;
    } catch (error) {
      if (sequence !== loadSequence || deps.current()?.id !== segment.id) return;
      state.readyId = null;
      state.loadError = formatPublicErrorReference(error) ?? get(t)('errors.unknown');
      notifications.error(get(t)('review.draftLoadFailed'), {
        cause: error,
        publicDetail: api.reviewErrorMessage(error, get(t)('review.draftLoadFailedHint')),
      });
    }
  }

  function activateSelection(segment: SpeechSegment, revision: number | null) {
    if (segment.id === state.lastLoadedId && revision === state.lastRevision) return false;
    cancelScheduledWrite();
    if (state.lastLoadedId) {
      const outgoingReadable = state.readyId === state.lastLoadedId && state.loadError === null;
      const text = deps.editText();
      if (text.trim() !== state.lastOriginal.trim()) editCache.set(state.lastLoadedId, text);
      else editCache.delete(state.lastLoadedId);
      if (
        state.lastRevision !== null &&
        outgoingReadable &&
        (!state.conflict || text.trim() !== state.lastOriginal.trim())
      ) {
        void queueWrite(state.lastLoadedId, state.lastRevision, text, state.lastOriginal).catch(
          () => undefined,
        );
      }
    }
    state.readyId = null;
    state.conflict = null;
    state.loadError = null;
    state.recovered = false;
    state.saveFailed = false;
    state.lastLoadedId = segment.id;
    state.lastOriginal = deps.originalText(segment);
    state.lastRevision = revision;
    deps.setEditText(editCache.get(segment.id) ?? state.lastOriginal);
    deps.onSelectionActivated(segment);
    if (revision !== null) void load(segment, revision, deps.editText());
    return true;
  }

  function scheduleActiveWrite(segment: SpeechSegment | null) {
    cancelScheduledWrite();
    if (!segment || state.readyId !== segment.id || state.lastRevision === null) return;
    const requested = intent(segment.id, state.lastRevision, deps.editText(), state.lastOriginal);
    if (writes.isDurable(requested)) return;
    const generation = scheduledWriteGeneration;
    const timer = window.setTimeout(() => {
      if (generation !== scheduledWriteGeneration || scheduledWriteTimer !== timer) return;
      scheduledWriteTimer = null;
      void writes.request(requested).catch(() => undefined);
    }, 500);
    scheduledWriteTimer = timer;
    return () => {
      if (generation !== scheduledWriteGeneration || scheduledWriteTimer !== timer) return;
      cancelScheduledWrite();
    };
  }

  async function flush(): Promise<void> {
    cancelScheduledWrite();
    if (state.lastLoadedId && (state.readyId !== state.lastLoadedId || state.loadError !== null)) {
      throw new Error(get(t)('inbox.disabled.draftUnavailable'));
    }
    if (state.lastLoadedId && state.lastRevision !== null) {
      if (!state.conflict || deps.editText().trim() !== state.lastOriginal.trim()) {
        await queueWrite(
          state.lastLoadedId,
          state.lastRevision,
          deps.editText(),
          state.lastOriginal,
        );
      }
    }
    await writes.flushAll();
  }

  function useConflict() {
    const current = deps.current();
    const conflict = state.conflict;
    if (
      !current ||
      !conflict ||
      current.id !== conflict.segmentId ||
      state.lastLoadedId !== conflict.segmentId ||
      state.readyId !== conflict.segmentId
    )
      return;
    deps.setEditText(conflict.text);
    editCache.set(current.id, conflict.text);
    state.conflict = null;
    state.recovered = true;
  }

  async function retry(baseRevision: number | undefined) {
    const current = deps.current();
    if (!current || !Number.isSafeInteger(baseRevision) || (baseRevision ?? -1) < 0) return;
    state.readyId = null;
    state.loadError = null;
    await load(current, baseRevision as number, deps.editText());
  }

  function discardConflict() {
    const current = deps.current();
    const conflict = state.conflict;
    if (!current || !conflict) return;
    showConfirmDialog.set({
      title: get(t)('review.discardDraftConfirmTitle'),
      message: get(t)('review.discardDraftConfirmMessage'),
      confirmLabel: get(t)('review.discardLocalDraft'),
      danger: true,
      onConfirm: async () => {
        const active = state.conflict;
        if (
          deps.current()?.id !== conflict.segmentId ||
          !active ||
          active.segmentId !== conflict.segmentId ||
          active.baseRevision !== conflict.baseRevision ||
          active.updatedAt !== conflict.updatedAt ||
          active.text !== conflict.text
        )
          return;
        try {
          cancelScheduledWrite();
          await writes.flushSegment(conflict.segmentId);
          const deleted = await api.deleteReviewDraftV1(conflict.segmentId, conflict.baseRevision);
          if (deleted !== true) {
            const remaining = await api.getReviewDraftV1(conflict.segmentId);
            if (remaining) {
              if (state.conflict === active && deps.current()?.id === conflict.segmentId) {
                state.conflict = remaining;
              }
              throw new Error('E_REVIEW_DRAFT_DELETE_NOT_CONFIRMED');
            }
          }
          if (state.conflict === active && deps.current()?.id === conflict.segmentId) {
            writes.acknowledge({
              kind: 'delete',
              segmentId: conflict.segmentId,
              baseRevision: conflict.baseRevision,
            });
            editCache.delete(conflict.segmentId);
            deps.setEditText(state.lastOriginal);
            state.conflict = null;
            state.recovered = false;
          }
        } catch (error) {
          notifications.error(get(t)('review.draftDiscardFailed'), {
            cause: error,
            publicDetail: api.reviewErrorMessage(error, get(t)('review.draftDiscardFailed')),
          });
        }
      },
    });
  }

  function acknowledgeCommitted(segmentId: string, baseRevision: number) {
    cancelScheduledWrite();
    editCache.delete(segmentId);
    writes.acknowledge({ kind: 'delete', segmentId, baseRevision });
    if (
      deps.current()?.id === segmentId &&
      state.lastLoadedId === segmentId &&
      state.lastRevision === baseRevision
    ) {
      // The server has durably committed this exact visible text and deleted its draft. Treat that
      // text as the temporary baseline until the authoritative queue reload replaces the row, so
      // the projection consumer's mandatory flush cannot recreate the just-consumed draft.
      state.lastOriginal = deps.editText();
      state.recovered = false;
      state.saveFailed = false;
    }
  }

  async function discardActiveEdit(
    segmentId: string,
    baseRevision: number,
    expectedText: string,
  ): Promise<boolean> {
    if (
      deps.current()?.id !== segmentId ||
      state.lastLoadedId !== segmentId ||
      state.readyId !== segmentId ||
      state.lastRevision !== baseRevision ||
      deps.editText() !== expectedText
    )
      return false;
    cancelScheduledWrite();
    await writes.request({ kind: 'delete', segmentId, baseRevision });
    if (
      deps.current()?.id !== segmentId ||
      state.lastLoadedId !== segmentId ||
      state.lastRevision !== baseRevision ||
      deps.editText() !== expectedText
    )
      return false;
    editCache.delete(segmentId);
    deps.setEditText(state.lastOriginal);
    state.recovered = false;
    state.saveFailed = false;
    return true;
  }

  function setBaseline(text: string) {
    cancelScheduledWrite();
    state.lastOriginal = text;
  }

  function forget(segmentId: string) {
    if (state.lastLoadedId === segmentId) cancelScheduledWrite();
    editCache.delete(segmentId);
    if (state.lastLoadedId === segmentId) state.lastLoadedId = null;
  }

  return {
    state,
    blockedKey,
    activateSelection,
    scheduleActiveWrite,
    flush,
    useConflict,
    retry,
    discardConflict,
    discardActiveEdit,
    acknowledgeCommitted,
    setBaseline,
    forget,
  };
}

export type ReviewModeDraftController = ReturnType<typeof createReviewModeDraftController>;
