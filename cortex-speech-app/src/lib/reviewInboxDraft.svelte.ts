import { get } from 'svelte/store';
import { tick } from 'svelte';
import * as api from './commands';
import { t, type TranslationKey } from './i18n';
import {
  ReviewDraftWriteCoordinator,
  type RevisionBoundReviewDraftIntent,
} from './reviewDraftWriteCoordinator';
import { reviewTranscript } from './reviewTranscriptAuthority';
import { showConfirmDialog } from './stores/uiStore';
import type { ReviewDraftV1 } from './commands';
import type { SpeechSegment } from './types';

interface DraftDependencies {
  current: () => SpeechSegment | null;
  currentRevision: () => number | undefined;
  resetSelectionAuthority: () => void;
  setStatus: (message: string) => void;
}

export function createReviewInboxDraftController(deps: DraftDependencies) {
  const state = $state({
    editing: false,
    editText: '',
    textarea: null as HTMLTextAreaElement | null,
    editingForId: null as string | null,
    baseline: '',
    readyId: null as string | null,
    conflict: null as ReviewDraftV1 | null,
    recovered: false,
    saving: false,
    saveFailed: false,
    loadError: null as string | null,
    pending: false,
  });
  let loadSequence = 0;
  let selectionGeneration = 0;
  let activeSelectionKey: string | null = null;
  let saveTimer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;
  const writes = new ReviewDraftWriteCoordinator({
    save: api.saveReviewDraftV1,
    delete: api.deleteReviewDraftV1,
    onStateChange: refreshSaving,
    onWriteSucceeded: (intent) => {
      if (disposed) return;
      if (
        isActiveTarget(intent.segmentId, intent.baseRevision) &&
        !writes.hasDesired(intent.segmentId)
      )
        state.saveFailed = false;
    },
    onWriteFailed: (intent) => {
      if (disposed) return;
      if (!isActiveTarget(intent.segmentId, intent.baseRevision)) return;
      state.saveFailed = true;
      deps.setStatus(get(t)('review.draftSaveFailedHint'));
    },
  });

  function blockedKey(): TranslationKey | null {
    const current = deps.current();
    if (!current) return null;
    if (state.loadError) return 'inbox.disabled.draftUnavailable';
    if (state.readyId !== current.id) return 'inbox.disabled.draftLoading';
    if (state.conflict) return 'inbox.disabled.draftConflict';
    if (state.pending && !state.editing) return 'inbox.disabled.draftPending';
    return null;
  }

  function isActiveTarget(segmentId: string, baseRevision: number) {
    return !disposed && deps.current()?.id === segmentId && deps.currentRevision() === baseRevision;
  }

  function isCurrentLoad(
    sequence: number,
    generation: number,
    segmentId: string,
    baseRevision: number,
  ) {
    return (
      !disposed &&
      sequence === loadSequence &&
      generation === selectionGeneration &&
      isActiveTarget(segmentId, baseRevision)
    );
  }

  function refreshSaving() {
    if (disposed) return;
    const current = deps.current();
    state.saving = !!current && writes.isWriting(current.id);
  }

  function clearTimer() {
    if (saveTimer === null) return;
    clearTimeout(saveTimer);
    saveTimer = null;
  }

  function draftIntent(
    segmentId: string,
    baseRevision: number,
    text: string,
    baseline: string,
  ): RevisionBoundReviewDraftIntent {
    return text.trim() === baseline.trim()
      ? { kind: 'delete', segmentId, baseRevision }
      : { kind: 'save', segmentId, baseRevision, text };
  }

  function queueWrite(segmentId: string, baseRevision: number, text: string, baseline: string) {
    return writes.request(draftIntent(segmentId, baseRevision, text, baseline));
  }

  function scheduleSave() {
    if (disposed) return;
    clearTimer();
    const segmentId = state.editingForId;
    const revision = deps.currentRevision();
    if (
      !state.editing ||
      !segmentId ||
      deps.current()?.id !== segmentId ||
      typeof revision !== 'number' ||
      state.readyId !== segmentId
    )
      return;
    const text = state.editText;
    const baseline = state.baseline;
    saveTimer = setTimeout(() => {
      saveTimer = null;
      if (disposed) return;
      void queueWrite(segmentId, revision, text, baseline).catch(() => undefined);
    }, 500);
  }

  function handleInput(text: string) {
    if (disposed) return;
    state.editText = text;
    state.pending = text.trim() !== state.baseline.trim();
    scheduleSave();
  }

  async function load(segment: SpeechSegment, baseRevision: number, generation: number) {
    if (disposed) return;
    const sequence = ++loadSequence;
    try {
      await writes.flushSegment(segment.id);
      if (!isCurrentLoad(sequence, generation, segment.id, baseRevision)) return;
      const draft = await api.getReviewDraftV1(segment.id);
      if (!isCurrentLoad(sequence, generation, segment.id, baseRevision)) return;
      state.conflict = null;
      state.recovered = false;
      state.saveFailed = false;
      state.loadError = null;
      if (!draft) {
        writes.acknowledge({ kind: 'delete', segmentId: segment.id, baseRevision });
      } else if (draft.segmentId !== segment.id) {
        throw new Error(get(t)('inbox.error.draftIdentityMismatch'));
      } else if (draft.baseRevision === baseRevision) {
        if (draft.text.trim() === state.baseline.trim()) {
          // This cleanup is a write. Re-check the lifecycle immediately before reserving it so an
          // Inbox destroyed while the read was outstanding cannot delete a same-revision draft
          // saved by the replacement surface.
          if (!isCurrentLoad(sequence, generation, segment.id, baseRevision)) return;
          await writes.request({ kind: 'delete', segmentId: segment.id, baseRevision });
          if (!isCurrentLoad(sequence, generation, segment.id, baseRevision)) return;
        } else {
          writes.acknowledge({
            kind: 'save',
            segmentId: segment.id,
            baseRevision,
            text: draft.text,
          });
        }
        state.editText = draft.text;
        state.pending = draft.text.trim() !== state.baseline.trim();
        state.recovered = state.pending;
        state.editing = state.pending;
        state.editingForId = state.pending ? segment.id : null;
        if (state.editing)
          void tick().then(() => {
            if (isCurrentLoad(sequence, generation, segment.id, baseRevision)) {
              state.textarea?.focus();
            }
          });
      } else {
        state.conflict = draft;
      }
      state.readyId = segment.id;
    } catch (error) {
      if (!isCurrentLoad(sequence, generation, segment.id, baseRevision)) return;
      state.loadError = api.reviewErrorMessage(error, get(t)('review.draftLoadFailedHint'));
      deps.setStatus(get(t)('review.draftLoadFailed'));
      state.readyId = null;
    }
  }

  async function activate(segment: SpeechSegment | null, revision: number | undefined) {
    if (disposed) return;
    clearTimer();
    const generation = ++selectionGeneration;
    deps.resetSelectionAuthority();
    state.editing = false;
    state.editText = '';
    state.editingForId = null;
    state.baseline = '';
    state.readyId = null;
    state.conflict = null;
    state.recovered = false;
    state.saveFailed = false;
    state.loadError = null;
    state.pending = false;
    refreshSaving();
    if (!segment || typeof revision !== 'number') return;
    state.baseline = reviewTranscript(segment);
    state.editText = state.baseline;
    await load(segment, revision, generation);
  }

  function syncSelection() {
    if (disposed) return undefined;
    const current = deps.current();
    const revision = deps.currentRevision();
    const key = current
      ? `${current.id}\0${typeof revision === 'number' ? revision : 'missing'}`
      : null;
    if (key === activeSelectionKey) return undefined;
    activeSelectionKey = key;
    return activate(current, revision);
  }

  async function retryLoad() {
    if (disposed) return;
    const current = deps.current();
    const revision = deps.currentRevision();
    if (!current || typeof revision !== 'number') return;
    state.loadError = null;
    await load(current, revision, selectionGeneration);
  }

  function useConflict() {
    if (disposed) return;
    const current = deps.current();
    const conflict = state.conflict;
    if (
      !current ||
      !conflict ||
      current.id !== conflict.segmentId ||
      state.readyId !== conflict.segmentId ||
      state.conflict !== conflict ||
      typeof deps.currentRevision() !== 'number'
    )
      return;
    state.editText = conflict.text;
    state.editingForId = current.id;
    state.editing = true;
    state.pending = state.editText.trim() !== state.baseline.trim();
    state.recovered = true;
    state.conflict = null;
    scheduleSave();
    void tick().then(() => {
      if (!disposed) state.textarea?.focus();
    });
  }

  function discardConflict() {
    if (disposed) return;
    const current = deps.current();
    const conflict = state.conflict;
    if (!current || !conflict || current.id !== conflict.segmentId) return;
    showConfirmDialog.set({
      title: get(t)('review.discardDraftConfirmTitle'),
      message: get(t)('review.discardDraftConfirmMessage'),
      confirmLabel: get(t)('review.discardLocalDraft'),
      danger: true,
      onConfirm: async () => {
        if (disposed) return;
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
          clearTimer();
          await writes.flushSegment(conflict.segmentId);
          if (disposed) return;
          const deleted = await api.deleteReviewDraftV1(conflict.segmentId, conflict.baseRevision);
          if (disposed) return;
          if (deleted !== true) {
            const remaining = await api.getReviewDraftV1(conflict.segmentId);
            if (disposed) return;
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
            state.editText = state.baseline;
            state.pending = false;
            state.editing = false;
            state.editingForId = null;
            state.conflict = null;
            state.recovered = false;
          }
        } catch {
          if (disposed) return;
          state.saveFailed = true;
          deps.setStatus(get(t)('review.draftDiscardFailed'));
        }
      },
    });
  }

  async function flush() {
    if (disposed) return;
    clearTimer();
    const revision = deps.currentRevision();
    if (
      state.editing &&
      state.editingForId &&
      deps.current()?.id === state.editingForId &&
      typeof revision === 'number' &&
      state.readyId === state.editingForId
    ) {
      await queueWrite(state.editingForId, revision, state.editText, state.baseline);
    }
    await writes.flushAll();
  }

  async function startEdit() {
    if (disposed) return;
    const current = deps.current();
    if (
      !current ||
      state.editing ||
      state.readyId !== current.id ||
      state.loadError ||
      state.conflict
    )
      return;
    if (state.editingForId !== current.id) state.editText = state.baseline;
    state.editing = true;
    state.editingForId = current.id;
    await tick();
    if (disposed) return;
    state.textarea?.focus();
    state.textarea?.select();
  }

  async function cancelEdit() {
    if (disposed) return;
    try {
      await flush();
      if (disposed) return;
      state.editing = false;
      if (state.pending) deps.setStatus(get(t)('inbox.status.draftKept'));
    } catch {
      if (disposed) return;
      deps.setStatus(get(t)('review.draftSaveFailedHint'));
      void tick().then(() => state.textarea?.focus());
    }
  }

  function acknowledgeCommitted(segmentId: string, baseRevision: number) {
    if (disposed) return;
    clearTimer();
    writes.acknowledge({ kind: 'delete', segmentId, baseRevision });
  }

  function finishCommit(segmentId: string, baseRevision: number) {
    if (disposed) return;
    acknowledgeCommitted(segmentId, baseRevision);
    if (deps.current()?.id !== segmentId) return;
    state.editing = false;
    state.pending = false;
    state.recovered = false;
    state.editingForId = null;
  }

  function dispose() {
    if (disposed) return;
    disposed = true;
    clearTimer();
    ++loadSequence;
    ++selectionGeneration;
    activeSelectionKey = null;
  }

  return {
    state,
    blockedKey,
    syncSelection,
    retryLoad,
    useConflict,
    discardConflict,
    flush,
    startEdit,
    cancelEdit,
    handleInput,
    acknowledgeCommitted,
    finishCommit,
    dispose,
  };
}

export type ReviewInboxDraftController = ReturnType<typeof createReviewInboxDraftController>;
