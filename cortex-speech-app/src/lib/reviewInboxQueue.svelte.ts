import { get } from 'svelte/store';
import { tick } from 'svelte';
import * as api from './commands';
import { physicalKey } from './keyboard';
import { t } from './i18n';
import type { ReviewPageV1 } from './commands';
import type { SpeechSegment } from './types';
import { ProjectionEpoch } from './projectionEpoch';

interface QueueDependencies {
  flushDraft: () => Promise<void>;
  resetSessionAuthority: () => void;
  setStatus: (message: string) => void;
  publicError: (error: unknown) => string;
  focusEditor: () => void;
  navigationBlocked: () => boolean;
}

type Eligibility = { eligible: boolean; disabledReason: string | null };

export function createReviewInboxQueueController(deps: QueueDependencies) {
  const pageSize = 200;
  const maxResidentRows = pageSize * 3;
  const nearEndThreshold = 10;
  const state = $state({
    rows: [] as SpeechSegment[],
    revisions: {} as Record<string, number>,
    eligibility: {} as Record<string, Eligibility>,
    nextCursor: null as string | null,
    total: 0,
    loadingMore: false,
    loadMoreError: null as string | null,
    evictedCount: 0,
    index: 0,
    loading: false,
    loadError: null as string | null,
    announcedIndex: null as number | null,
    listbox: null as HTMLUListElement | null,
  });
  const projection = new ProjectionEpoch();
  let loadGeneration = 0;
  let navigationSequence = 0;
  let loadMorePromise: Promise<boolean> | null = null;

  const current = () => state.rows[state.index] ?? null;
  const currentRevision = () => {
    const row = current();
    return row ? state.revisions[row.id] : undefined;
  };
  const currentEligibility = () => {
    const row = current();
    return row ? state.eligibility[row.id] : null;
  };
  const pendingCount = () => state.rows.filter((row) => !row.humanDecision).length;
  const activeAnnouncement = () => {
    const index = state.announcedIndex;
    return index == null || !state.rows[index]
      ? ''
      : get(t)('inbox.activeItem', {
          position: String(index + 1),
          total: String(state.rows.length),
        });
  };

  $effect(() => {
    if (state.index >= 0) void scrollToCurrent();
  });

  async function scrollToCurrent() {
    await tick();
    state.listbox
      ?.querySelector<HTMLElement>('[role="option"][aria-selected="true"]')
      ?.scrollIntoView({ block: 'nearest' });
  }

  async function loadAttempt(authoritativeReconciliation = false): Promise<number | null> {
    if (!authoritativeReconciliation && deps.navigationBlocked()) {
      deps.setStatus(get(t)('inbox.disabled.saving'));
      return null;
    }
    const projectionEpoch = projection.begin();
    try {
      await deps.flushDraft();
    } catch {
      if (projection.isLatest(projectionEpoch)) {
        deps.setStatus(get(t)('review.closeDraftFailed'));
      }
      projection.finish(projectionEpoch, false);
      return null;
    }
    if (!projection.isLatest(projectionEpoch)) {
      return projection.settle(projectionEpoch, false);
    }
    if (!authoritativeReconciliation && deps.navigationBlocked()) {
      deps.setStatus(get(t)('inbox.disabled.saving'));
      return projection.settle(projectionEpoch, false);
    }
    const generation = ++loadGeneration;
    state.loading = true;
    state.loadingMore = false;
    state.nextCursor = null;
    state.loadError = null;
    state.loadMoreError = null;
    try {
      const page = await api.getReviewPageV1({ kind: 'escalation' }, null, pageSize);
      if (
        generation !== loadGeneration ||
        !projection.isLatest(projectionEpoch) ||
        (!authoritativeReconciliation && deps.navigationBlocked())
      ) {
        return projection.settle(projectionEpoch, false);
      }
      // A reviewer may navigate while this authoritative reload is in flight (for example while a
      // commit response is settling). Preserve that live selection when it still exists in the
      // refreshed page; resetting to row zero can silently move focus back to the clip just decided.
      const selectedId = current()?.id ?? null;
      state.rows = page.items.map((item) => item.segment);
      state.revisions = Object.fromEntries(
        page.items.map((item) => [item.segment.id, item.baseRevision]),
      );
      state.eligibility = Object.fromEntries(
        page.items.map((item) => [
          item.segment.id,
          { eligible: item.eligible, disabledReason: item.disabledReason },
        ]),
      );
      state.total = page.total;
      state.nextCursor = page.nextCursor;
      state.evictedCount = 0;
      const selectedIndex = selectedId ? state.rows.findIndex((row) => row.id === selectedId) : -1;
      state.index = selectedIndex >= 0 ? selectedIndex : 0;
      state.announcedIndex = null;
      deps.resetSessionAuthority();
      return projection.settle(projectionEpoch, true);
    } catch (error) {
      if (
        generation !== loadGeneration ||
        !projection.isLatest(projectionEpoch) ||
        (!authoritativeReconciliation && deps.navigationBlocked())
      ) {
        return projection.settle(projectionEpoch, false);
      }
      state.loadError = get(t)('inbox.status.loadFailed', { err: deps.publicError(error) });
      deps.setStatus(state.loadError);
      return projection.settle(projectionEpoch, false);
    } finally {
      if (generation === loadGeneration) state.loading = false;
    }
  }

  async function load() {
    if (deps.navigationBlocked()) {
      deps.setStatus(get(t)('inbox.disabled.saving'));
      return false;
    }
    return (await loadAttempt()) !== null;
  }

  async function reloadProjection(): Promise<number | null> {
    // Projection reconciliation is part of the durable truth barrier and is therefore allowed to
    // refresh the queue while ordinary reload/navigation intents remain frozen.
    return loadAttempt(true);
  }

  function projectionReceipt(): number | null {
    return projection.receipt();
  }

  function merge(page: ReviewPageV1) {
    const selectedId = current()?.id ?? null;
    const combined = [...state.rows];
    const indexById = new Map(combined.map((row, index) => [row.id, index]));
    const revisions = { ...state.revisions };
    const eligibility = { ...state.eligibility };
    for (const item of page.items) {
      const existingIndex = indexById.get(item.segment.id);
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
    if (combined.length > maxResidentRows) {
      const retainedIds = new Set(
        combined.slice(combined.length - maxResidentRows).map((row) => row.id),
      );
      if (selectedId && !retainedIds.has(selectedId)) {
        const oldestNewerId = combined.find((row) => retainedIds.has(row.id))?.id;
        if (oldestNewerId) retainedIds.delete(oldestNewerId);
        retainedIds.add(selectedId);
      }
      retained = combined.filter((row) => retainedIds.has(row.id));
      state.evictedCount += combined.length - retained.length;
    }
    const retainedIds = new Set(retained.map((row) => row.id));
    state.revisions = Object.fromEntries(
      Object.entries(revisions).filter(([id]) => retainedIds.has(id)),
    );
    state.eligibility = Object.fromEntries(
      Object.entries(eligibility).filter(([id]) => retainedIds.has(id)),
    );
    state.rows = retained;
    const selectedIndex = selectedId ? retained.findIndex((row) => row.id === selectedId) : -1;
    state.index = selectedIndex >= 0 ? selectedIndex : Math.min(state.index, retained.length - 1);
    if (state.index < 0) state.index = 0;
    if (state.announcedIndex !== null) state.announcedIndex = state.index;
  }

  async function loadMoreAttempt(): Promise<boolean> {
    if (deps.navigationBlocked()) {
      deps.setStatus(get(t)('inbox.disabled.saving'));
      return false;
    }
    const cursor = state.nextCursor;
    if (!cursor || state.loading) return false;
    const projectionEpoch = projection.begin();
    const generation = loadGeneration;
    let succeeded = false;
    state.loadingMore = true;
    state.loadMoreError = null;
    try {
      const page = await api.getReviewPageV1({ kind: 'escalation' }, cursor, pageSize);
      if (
        generation !== loadGeneration ||
        cursor !== state.nextCursor ||
        !projection.isLatest(projectionEpoch) ||
        deps.navigationBlocked()
      )
        return false;
      merge(page);
      state.total = page.total;
      state.nextCursor = page.nextCursor === cursor ? null : page.nextCursor;
      succeeded = true;
      return true;
    } catch (error) {
      if (
        generation !== loadGeneration ||
        cursor !== state.nextCursor ||
        !projection.isLatest(projectionEpoch)
      )
        return false;
      state.loadMoreError = get(t)('inbox.status.loadMoreFailed', {
        err: deps.publicError(error),
      });
      deps.setStatus(state.loadMoreError);
      return false;
    } finally {
      if (generation === loadGeneration) state.loadingMore = false;
      projection.finish(projectionEpoch, succeeded);
    }
  }

  /** Coalesce prefetch and explicit navigation onto the same authoritative page request. */
  function loadMore(): Promise<boolean> {
    if (loadMorePromise) return loadMorePromise;
    const attempt = loadMoreAttempt();
    loadMorePromise = attempt;
    const clear = () => {
      if (loadMorePromise === attempt) loadMorePromise = null;
    };
    void attempt.then(clear, clear);
    return attempt;
  }

  function maybeLoadMore(index: number) {
    if (state.nextCursor && index >= state.rows.length - nearEndThreshold) void loadMore();
  }

  async function focusListbox() {
    await tick();
    state.listbox?.focus({ preventScroll: true });
  }

  async function select(index: number, announce: boolean, focus: boolean) {
    if (deps.navigationBlocked()) {
      deps.setStatus(get(t)('inbox.disabled.saving'));
      return false;
    }
    if (state.rows.length === 0) return false;
    const next = Math.max(0, Math.min(index, state.rows.length - 1));
    const sequence = ++navigationSequence;
    const targetId = state.rows[next]?.id ?? null;
    if (next === state.index) {
      if (focus) void focusListbox();
      maybeLoadMore(next);
      return true;
    }
    try {
      await deps.flushDraft();
    } catch {
      deps.setStatus(get(t)('review.closeDraftFailed'));
      void tick().then(deps.focusEditor);
      return false;
    }
    if (sequence !== navigationSequence || deps.navigationBlocked() || targetId === null) {
      if (deps.navigationBlocked()) deps.setStatus(get(t)('inbox.disabled.saving'));
      return false;
    }
    const resolvedIndex = state.rows.findIndex((row) => row.id === targetId);
    if (resolvedIndex < 0) return false;
    // No await can interleave between the final barrier check and this identity-bound write.
    state.index = resolvedIndex;
    if (announce) state.announcedIndex = resolvedIndex;
    if (focus) void focusListbox();
    maybeLoadMore(resolvedIndex);
    return true;
  }

  async function advance() {
    if (state.index < state.rows.length - 1) {
      await select(state.index + 1, true, false);
      return;
    }
    const activeId = current()?.id ?? null;
    if (!state.nextCursor) return;
    await loadMore();
    const activeIndex = activeId ? state.rows.findIndex((row) => row.id === activeId) : -1;
    if (activeIndex >= 0 && activeIndex < state.rows.length - 1) {
      await select(activeIndex + 1, true, false);
    }
  }

  function canAdvance() {
    return state.index < state.rows.length - 1 || state.nextCursor !== null;
  }

  function optionId(index: number) {
    return `review-inbox-option-${index}`;
  }

  function optionLabel(segment: SpeechSegment, index: number) {
    return get(t)(segment.humanDecision ? 'inbox.queueItemReviewed' : 'inbox.queueItem', {
      position: String(index + 1),
      total: String(state.rows.length),
      id: segment.id,
    });
  }

  function handleListboxKey(event: KeyboardEvent) {
    const key = physicalKey(event);
    let next: number | null = null;
    if (key === 'ArrowDown' || key === 'ArrowRight') next = state.index + 1;
    else if (key === 'ArrowUp' || key === 'ArrowLeft') next = state.index - 1;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = state.rows.length - 1;
    if (next == null) return;
    event.preventDefault();
    event.stopPropagation();
    void select(next, true, true);
  }

  function handleOptionKey(event: KeyboardEvent, index: number) {
    if (event.key !== 'Enter' && event.key !== ' ') return;
    event.preventDefault();
    event.stopPropagation();
    void select(index, true, true);
  }

  function applyCommittedRow(segment: SpeechSegment) {
    projection.mutate();
    const index = state.rows.findIndex((row) => row.id === segment.id);
    if (index >= 0) state.rows = state.rows.map((row, i) => (i === index ? segment : row));
  }

  return {
    state,
    current,
    currentRevision,
    currentEligibility,
    pendingCount,
    activeAnnouncement,
    load,
    reloadProjection,
    projectionReceipt,
    loadMore,
    select,
    advance,
    canAdvance,
    optionId,
    optionLabel,
    handleListboxKey,
    handleOptionKey,
    applyCommittedRow,
  };
}

export type ReviewInboxQueueController = ReturnType<typeof createReviewInboxQueueController>;
