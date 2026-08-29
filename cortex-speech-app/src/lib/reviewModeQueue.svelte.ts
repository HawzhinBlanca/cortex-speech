import { get } from 'svelte/store';
import * as api from './commands';
import { formatPublicErrorReference } from './errorText';
import { t } from './i18n';
import { isPlaceholderTranscript } from './segmentQuality';
import { segments } from './stores/segmentStore';
import { notifications } from './stores/notificationStore';
import type { SpeechSegment } from './types';
import { ProjectionEpoch } from './projectionEpoch';

type Eligibility = { eligible: boolean; disabledReason: string | null };
type HydrationAttempt = {
  generation: number;
  baseRevision: number;
  promise: Promise<number | null>;
};

export interface ReviewModeQueueState {
  suspectFirst: boolean;
  rows: SpeechSegment[];
  revisions: Record<string, number>;
  eligibility: Record<string, Eligibility>;
  cursor: string | null;
  total: number;
  initialTotal: number;
  corpusTotal: number;
  initiallyVerified: number;
  loading: boolean;
  loadError: string | null;
  hydratedIds: Set<string>;
  focusNarrowed: boolean;
  index: number;
  cursorRestored: boolean;
}

export function createReviewModeQueueController() {
  const state = $state<ReviewModeQueueState>({
    suspectFirst: false,
    rows: [],
    revisions: {},
    eligibility: {},
    cursor: null,
    total: 0,
    initialTotal: 0,
    corpusTotal: 0,
    initiallyVerified: 0,
    loading: false,
    loadError: null,
    hydratedIds: new Set(),
    focusNarrowed: false,
    index: 0,
    cursorRestored: false,
  });
  const hydrationInFlight = new Map<string, HydrationAttempt>();
  const projection = new ProjectionEpoch();
  let generation = 0;
  let loadKey = '';
  let search = '';
  let projectionHealthy = true;
  let disposed = false;
  let navigationBlocked = () => false;

  const queue = () => state.rows;
  const currentCandidate = () => queue()[state.index] ?? null;
  const current = () => {
    const candidate = currentCandidate();
    return candidate && state.hydratedIds.has(candidate.id) ? candidate : null;
  };
  const currentEligibility = (): Eligibility | null => {
    const row = current();
    return row
      ? (state.eligibility[row.id] ?? {
          eligible: false,
          disabledReason: 'REVIEW_ELIGIBILITY_UNKNOWN',
        })
      : null;
  };
  const searchScoped = () => search.trim().length > 0;
  const subsetScoped = () => searchScoped() || state.focusNarrowed;
  const progress = () => {
    const subset = subsetScoped();
    const total = subset ? state.initialTotal : state.corpusTotal;
    const done = subset
      ? Math.max(0, state.initialTotal - state.total)
      : Math.min(
          state.corpusTotal,
          state.initiallyVerified + Math.max(0, state.initialTotal - state.total),
        );
    return {
      done,
      total,
      percent: total > 0 ? Math.round((done / total) * 100) : 0,
      allReviewed: !subset && state.corpusTotal > 0 && state.total === 0,
    };
  };

  async function hydrate(id: string, force = false): Promise<number | null> {
    if (disposed) return null;
    if (!force && state.hydratedIds.has(id)) return projection.receipt();
    const attemptGeneration = generation;
    const baseRevision = state.revisions[id];
    if (!Number.isSafeInteger(baseRevision) || baseRevision < 0) {
      throw new Error('review row hydration requires the exact rendered revision');
    }
    const existing = hydrationInFlight.get(id);
    if (
      existing &&
      existing.generation === attemptGeneration &&
      existing.baseRevision === baseRevision
    ) {
      return existing.promise;
    }
    const promise = (async () => {
      try {
        const full = await api.getSegment(id);
        if (
          disposed ||
          attemptGeneration !== generation ||
          state.revisions[id] !== baseRevision ||
          !state.rows.some((row) => row.id === id)
        ) {
          return null;
        }
        const projectionEpoch = projection.begin();
        state.rows = state.rows.map((row) => (row.id === id ? full : row));
        segments.update((rows) => rows.map((row) => (row.id === id ? full : row)));
        state.hydratedIds = new Set([...state.hydratedIds, id]);
        return projection.settle(projectionEpoch, true);
      } catch (error) {
        if (disposed || attemptGeneration !== generation) return null;
        throw error;
      }
    })();
    const attempt = { generation: attemptGeneration, baseRevision, promise };
    hydrationInFlight.set(id, attempt);
    projectionHealthy = false;
    let receipt: number | null = null;
    try {
      receipt = await promise;
      return receipt;
    } finally {
      if (hydrationInFlight.get(id) === attempt) {
        hydrationInFlight.delete(id);
        if (!disposed && hydrationInFlight.size === 0) projectionHealthy = receipt !== null;
      }
    }
  }

  async function loadAttempt(
    reset: boolean,
    authoritativeReconciliation = false,
  ): Promise<number | null> {
    if (disposed) return null;
    if (!authoritativeReconciliation && navigationBlocked()) return null;
    if (!reset && (state.loading || !state.cursor)) return null;
    const attemptGeneration = reset ? ++generation : generation;
    const cursor = reset ? null : state.cursor;
    const query = search.trim() || null;
    if (reset) {
      // Retire every hydration owned by the previous generation. Their promises cannot be cancelled,
      // but clearing the identity map ensures a late stale `finally` cannot poison the health of a
      // newer successful hydration (or delete a replacement attempt for the same segment).
      hydrationInFlight.clear();
      projection.mutate();
      projectionHealthy = false;
      state.rows = [];
      state.revisions = {};
      state.eligibility = {};
      state.cursor = null;
      state.total = 0;
      state.initialTotal = 0;
      state.corpusTotal = 0;
      state.initiallyVerified = 0;
      state.loadError = null;
      state.hydratedIds = new Set();
      state.index = 0;
    }
    state.loading = true;
    try {
      const statsPromise = reset && !query ? api.getDatasetStats().catch(() => null) : null;
      let pageRows: SpeechSegment[];
      let pageRevisions: Record<string, number>;
      let pageEligibility: Record<string, Eligibility>;
      let nextCursor: string | null;
      let total: number;
      let focusNarrowed: boolean;
      if (state.suspectFirst) {
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
        pageEligibility = Object.fromEntries(
          page.items.map((item) => {
            const eligible =
              Number.isSafeInteger(pageRevisions[item.id]) &&
              pageRevisions[item.id] >= 0 &&
              !isPlaceholderTranscript(item.rawTranscript);
            return [
              item.id,
              { eligible, disabledReason: eligible ? null : 'TRANSCRIPT_NOT_READY' },
            ];
          }),
        );
        nextCursor = page.nextCursor;
        total = page.total;
        focusNarrowed = page.focusNarrowed === true;
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
        nextCursor = page.nextCursor;
        total = page.total;
        focusNarrowed = page.focusNarrowed;
      }
      const stats = await statsPromise;
      if (
        disposed ||
        attemptGeneration !== generation ||
        (!reset && cursor !== state.cursor) ||
        (!authoritativeReconciliation && navigationBlocked())
      )
        return null;
      const projectionEpoch = projection.begin();
      state.loadError = null;
      state.rows = reset ? pageRows : [...state.rows, ...pageRows];
      state.revisions = reset ? pageRevisions : { ...state.revisions, ...pageRevisions };
      state.eligibility = reset ? pageEligibility : { ...state.eligibility, ...pageEligibility };
      state.cursor = nextCursor;
      state.focusNarrowed = focusNarrowed;
      if (reset) {
        state.total = total;
        state.initialTotal = total;
        state.corpusTotal = stats?.totalSegments ?? total;
        state.initiallyVerified = stats?.verifiedCount ?? 0;
        state.index = 0;
        projectionHealthy = hydrationInFlight.size === 0;
      }
      return projection.settle(projectionEpoch, true);
    } catch (error) {
      if (
        disposed ||
        attemptGeneration !== generation ||
        (!reset && cursor !== state.cursor) ||
        (!authoritativeReconciliation && navigationBlocked())
      )
        return null;
      state.loadError = formatPublicErrorReference(error) ?? get(t)('errors.unknown');
      if (reset) {
        state.rows = [];
        state.revisions = {};
        state.eligibility = {};
        state.cursor = null;
        state.total = 0;
        state.initialTotal = 0;
        projectionHealthy = false;
      }
      notifications.error(get(t)('notifications.loadSegmentsFailed'), { cause: error });
      return null;
    } finally {
      if (!disposed && attemptGeneration === generation) state.loading = false;
    }
  }

  async function load(reset: boolean) {
    return (await loadAttempt(reset)) !== null;
  }

  async function reloadProjection(): Promise<number | null> {
    if (disposed) return null;
    // This is the one queue read that must run while the durable truth barrier is held: it is the
    // barrier's authoritative reconciliation, not a user navigation or scope change.
    const loaded = await loadAttempt(true, true);
    if (loaded === null) return null;
    const candidate = currentCandidate();
    if (candidate) {
      try {
        return await hydrate(candidate.id);
      } catch {
        return null;
      }
    }
    return loaded;
  }

  function projectionReceipt(): number | null {
    return !disposed && projectionHealthy && hydrationInFlight.size === 0
      ? projection.receipt()
      : null;
  }

  function syncScope(nextSearch: string) {
    if (disposed || navigationBlocked()) return;
    search = nextSearch;
    const key = `${search.trim()}\0${state.suspectFirst ? 'suspect' : 'oldest'}`;
    if (key === loadKey) return;
    loadKey = key;
    void load(true);
  }

  function restoreCursor(selectedId: string | null) {
    if (disposed) return;
    if (state.cursorRestored || queue().length === 0) return;
    if (selectedId) {
      const position = queue().findIndex((row) => row.id === selectedId);
      if (position >= 0 && !queue()[position].verified) state.index = position;
    }
    state.cursorRestored = true;
  }

  function hydrateCandidate() {
    if (disposed) return;
    if (state.loading) return;
    const candidate = currentCandidate();
    if (!candidate || state.hydratedIds.has(candidate.id)) return;
    void hydrate(candidate.id).then(
      (receipt) => {
        if (
          !disposed &&
          receipt === null &&
          currentCandidate()?.id === candidate.id &&
          !state.hydratedIds.has(candidate.id)
        ) {
          queueMicrotask(hydrateCandidate);
        }
      },
      (error) => {
        if (disposed) return;
        state.loadError = formatPublicErrorReference(error) ?? get(t)('errors.unknown');
        notifications.error(get(t)('notifications.loadSegmentsFailed'), { cause: error });
      },
    );
  }

  function maybeLoadMore() {
    if (disposed) return;
    if (state.cursor && state.index >= queue().length - 10) void load(false);
  }

  function toggleSuspectFirst() {
    if (disposed || navigationBlocked()) return;
    projection.mutate();
    state.suspectFirst = !state.suspectFirst;
  }

  function dispose() {
    if (disposed) return;
    disposed = true;
    ++generation;
    hydrationInFlight.clear();
    projection.mutate();
    projectionHealthy = false;
  }

  function setNavigationBlocked(predicate: () => boolean) {
    navigationBlocked = predicate;
  }

  return {
    state,
    queue,
    currentCandidate,
    current,
    currentEligibility,
    searchScoped,
    subsetScoped,
    progress,
    hydrate,
    load,
    reloadProjection,
    projectionReceipt,
    syncScope,
    restoreCursor,
    hydrateCandidate,
    maybeLoadMore,
    toggleSuspectFirst,
    setNavigationBlocked,
    dispose,
  };
}

export type ReviewModeQueueController = ReturnType<typeof createReviewModeQueueController>;
