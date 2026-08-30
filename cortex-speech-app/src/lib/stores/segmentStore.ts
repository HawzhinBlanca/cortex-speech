import { tick } from 'svelte';
import { writable, derived, get } from 'svelte/store';
import type { SpeechSegment, WordTimestamp } from '../types';
import * as api from '../commands';
import { dedupeById } from '../dedupeById';
import { notifications } from './notificationStore';
import { t } from '../i18n';
import { formatPublicErrorReference } from '../errorText';
import { ProjectionEpoch } from '../projectionEpoch';

// A bounded render window. More rows are appended only as the virtual list approaches its end.
const PAGE_SIZE = 200;
const MAX_RESIDENT_PAGES = 3;
const MAX_RESIDENT_SEGMENTS = PAGE_SIZE * MAX_RESIDENT_PAGES;

function createSegmentsStore() {
  const { subscribe, set: rawSet, update: rawUpdate } = writable<SpeechSegment[]>([]);
  const projection = new ProjectionEpoch();
  let loadSeq = 0;
  let nextCursor: string | null = null;
  let loadingMore = false;
  let activeFullLoad: Promise<number | null> | null = null;
  const activeHydrations = new Map<string, Promise<SpeechSegment>>();
  const hydratedSegmentIds = new Set<string>();
  let hydrationTail: Promise<void> = Promise.resolve();
  let hydrationGeneration = 0;
  let projectionHealthy = true;

  function retireHydrations() {
    ++hydrationGeneration;
    activeHydrations.clear();
    hydrationTail = Promise.resolve();
  }

  function set(value: SpeechSegment[]) {
    retireHydrations();
    hydratedSegmentIds.clear();
    ++loadSeq;
    projection.mutate();
    projectionHealthy = true;
    rawSet(value);
  }

  function update(updater: (rows: SpeechSegment[]) => SpeechSegment[]) {
    retireHydrations();
    ++loadSeq;
    projection.mutate();
    projectionHealthy = true;
    rawUpdate(updater);
  }

  function bumpLoadGeneration() {
    retireHydrations();
    hydratedSegmentIds.clear();
    ++loadSeq;
    projection.mutate();
    projectionHealthy = true;
  }

  async function runLoadAttempt(): Promise<number | null> {
    // A full reload is the recovery boundary for a wedged row read. Retire cached/coalesced
    // hydrations immediately; late work is fenced by both this generation and the projection epoch.
    retireHydrations();
    hydratedSegmentIds.clear();
    projectionHealthy = false;
    const projectionEpoch = projection.begin();
    const seq = ++loadSeq;
    nextCursor = null;
    loadingMore = false;
    const sort = get(sortOrder);
    const verified = get(filterVerified);
    const query = get(searchQuery).trim() || null;
    try {
      const page = await api.getSegmentsPage({
        verified,
        query,
        sort,
        limit: PAGE_SIZE,
        cursor: null,
      });
      if (seq !== loadSeq || !projection.isLatest(projectionEpoch)) {
        return projection.settle(projectionEpoch, false);
      }
      rawSet(dedupeById(page.items));
      // Search is part of the server-side page scope now. Never retain a legacy frozen result set
      // across a reload, because it can hide newly matching rows or retain stale matches.
      searchResults.set(null);
      nextCursor = page.nextCursor;
      libraryTotal.set(page.total);
      libraryTruncated.set(nextCursor !== null);
      await Promise.all([refreshConformalThreshold(), refreshSegmentStats()]);
      // A newer load or local projection write may have superseded this run during metadata refresh.
      if (seq !== loadSeq || !projection.isLatest(projectionEpoch)) {
        return projection.settle(projectionEpoch, false);
      }
      libraryLoadError.set(null);
      projectionHealthy = true;
      return projection.settle(projectionEpoch, true);
    } catch (error) {
      if (seq !== loadSeq || !projection.isLatest(projectionEpoch)) {
        return projection.settle(projectionEpoch, false);
      }
      console.error('Failed to load segments', error);
      const message = formatPublicErrorReference(error) ?? get(t)('errors.unknown');
      libraryLoadError.set(message);
      notifications.error(get(t)('notifications.loadSegmentsFailed'), { cause: error });
      projectionHealthy = false;
      return projection.settle(projectionEpoch, false);
    }
  }

  function loadAttempt(): Promise<number | null> {
    const operation = runLoadAttempt();
    activeFullLoad = operation;
    void operation.then(
      () => {
        if (activeFullLoad === operation) activeFullLoad = null;
      },
      () => {
        if (activeFullLoad === operation) activeFullLoad = null;
      },
    );
    return operation;
  }

  function currentSegment(segmentId: string): SpeechSegment | null {
    return get({ subscribe }).find((row) => row.id === segmentId) ?? null;
  }

  /**
   * Replace a bounded page row with its complete backend projection. Duplicate requests for the
   * same row share one Promise. A page reload that begins later owns the projection, so a late
   * hydration can never put stale metadata back into the library.
   */
  function hydrate(segmentId: string): Promise<SpeechSegment> {
    const existing = activeHydrations.get(segmentId);
    if (existing) return existing;
    const generation = hydrationGeneration;

    const run = async (): Promise<SpeechSegment> => {
      if (generation !== hydrationGeneration) {
        const current = currentSegment(segmentId);
        if (current) return current;
        throw new Error('E_SEGMENT_HYDRATION_SUPERSEDED');
      }
      // A hydration triggered by publishing a page must not supersede any full load. Follow the
      // active pointer until no page load remains; if a newer load starts meanwhile it becomes the
      // authority and this row is hydrated only after that load settles.
      while (activeFullLoad) {
        const precedingLoad = activeFullLoad;
        await precedingLoad;
        if (activeFullLoad === precedingLoad) activeFullLoad = null;
      }
      // The await above is a retirement boundary: a newer reload may have completed and certified
      // its projection while this queued hydration was asleep. Never mint an epoch for retired work.
      if (generation !== hydrationGeneration) {
        const current = currentSegment(segmentId);
        if (current) return current;
        throw new Error('E_SEGMENT_HYDRATION_SUPERSEDED');
      }

      const seq = loadSeq;
      try {
        const hydrated = await api.getSegment(segmentId);
        if (generation !== hydrationGeneration || seq !== loadSeq) {
          const current = currentSegment(segmentId);
          if (current) return current;
          throw new Error('E_SEGMENT_HYDRATION_SUPERSEDED');
        }
        // Pagination and row hydration are compatible additive mutations. Mint the receipt only at
        // the synchronous apply point so a slow page fetch cannot retire an unrelated row fetch (or
        // vice versa); full reloads remain fenced by loadSeq/hydrationGeneration above.
        const projectionEpoch = projection.begin();
        hydratedSegmentIds.add(segmentId);
        rawUpdate((rows) => rows.map((row) => (row.id === segmentId ? hydrated : row)));
        projection.settle(projectionEpoch, true);
        return hydrated;
      } catch (error) {
        if (generation !== hydrationGeneration || seq !== loadSeq) {
          const current = currentSegment(segmentId);
          if (current) return current;
          throw new Error('E_SEGMENT_HYDRATION_SUPERSEDED', { cause: error });
        }
        throw error;
      }
    };

    // Different rows are deliberately serialized. Projection receipts attest a complete rendered
    // state; parallel last-writer-wins hydrations would let one successful row retire another row
    // that was still unresolved and could falsely certify a partial projection.
    const operation = hydrationTail.then(run, run);
    hydrationTail = operation.then(
      () => undefined,
      () => undefined,
    );

    activeHydrations.set(segmentId, operation);
    projectionHealthy = false;
    void operation.then(
      () => {
        if (activeHydrations.get(segmentId) === operation) {
          activeHydrations.delete(segmentId);
          if (activeHydrations.size === 0) projectionHealthy = true;
        }
      },
      () => {
        if (activeHydrations.get(segmentId) === operation) {
          activeHydrations.delete(segmentId);
          projectionHealthy = false;
        }
      },
    );
    return operation;
  }

  async function load() {
    return (await loadAttempt()) !== null;
  }

  async function reloadProjection(): Promise<number | null> {
    const receipt = await loadAttempt();
    if (receipt === null) return null;
    const seq = loadSeq;

    // Publishing a page can synchronously schedule component effects that request full rows. Let
    // those effects register, then require every registered hydration to settle successfully before
    // sealing one composite projection receipt for the undo/reconciliation barrier.
    await tick();
    while (activeHydrations.size > 0) {
      const hydrationResults = await Promise.allSettled([...activeHydrations.values()]);
      if (hydrationResults.some((result) => result.status === 'rejected')) return null;
      if (loadSeq !== seq) return null;
    }
    return loadSeq === seq ? projection.receipt() : null;
  }

  function projectionReceipt(): number | null {
    return projectionHealthy && activeHydrations.size === 0 ? projection.receipt() : null;
  }

  function isHydrated(segmentId: string): boolean {
    return hydratedSegmentIds.has(segmentId);
  }

  async function loadMore() {
    if (loadingMore || !nextCursor) return;
    const seq = loadSeq;
    const cursor = nextCursor;
    const sort = get(sortOrder);
    const verified = get(filterVerified);
    const query = get(searchQuery).trim() || null;
    loadingMore = true;
    try {
      const page = await api.getSegmentsPage({ verified, query, sort, limit: PAGE_SIZE, cursor });
      if (seq !== loadSeq || cursor !== nextCursor) return;
      const projectionEpoch = projection.begin();
      rawUpdate((current) => {
        const merged = dedupeById([...current, ...page.items]);
        return merged.length > MAX_RESIDENT_SEGMENTS
          ? merged.slice(merged.length - MAX_RESIDENT_SEGMENTS)
          : merged;
      });
      nextCursor = page.nextCursor;
      libraryTotal.set(page.total);
      libraryTruncated.set(nextCursor !== null);
      projection.settle(projectionEpoch, true);
    } catch (error) {
      if (seq === loadSeq) {
        notifications.error(get(t)('notifications.loadSegmentsFailed'), { cause: error });
      }
    } finally {
      if (seq === loadSeq) loadingMore = false;
    }
  }

  return {
    subscribe,
    set,
    update,
    bumpLoadGeneration,
    load,
    hydrate,
    reloadProjection,
    projectionReceipt,
    isHydrated,
    loadMore,
  };
}

export const segments = createSegmentsStore();
// True backend row count for the current server-side filter. `libraryTruncated` means another
// keyset page is available, not that rows were silently abandoned.
export const libraryTotal = writable(0);
export const libraryTruncated = writable(false);
// P2.1: non-null when the LAST library load() failed (the backend error message). Cleared on the next
// successful load. The empty view reads this to show a distinct "load failed + Retry" state instead of
// the "No segments loaded" hint, so a DB/IPC read error is never mistaken for an empty/wiped library.
export const libraryLoadError = writable<string | null>(null);
export const selectedSegmentId = writable<string | null>(null);
export const wordTimestamps = writable<WordTimestamp[]>([]);
export const filterVerified = writable<boolean | null>(null);
export const searchQuery = writable('');
export const searchResults = writable<SpeechSegment[] | null>(null);
export type SortOrder =
  'newest' | 'oldest' | 'duration' | 'verified' | 'confidence' | 'activeLearning';
export const sortOrder = writable<SortOrder>('newest');
export const conformalThreshold = writable<number>(0.35);

export async function refreshConformalThreshold(targetError = 0.05, confidence = 0.95) {
  try {
    const cert = await api.getDatasetCertificate(targetError, confidence);
    // Guard a null/malformed certificate: keep the current default threshold rather than throwing
    // (or setting NaN). A valid backend always returns a finite threshold; this defends against a
    // missing/partial response so segment loading never errors out on it.
    if (cert && typeof cert.threshold === 'number' && Number.isFinite(cert.threshold)) {
      conformalThreshold.set(cert.threshold);
    }
  } catch (e) {
    console.error('Failed to load conformal certificate', e);
  }
}

function segmentTimestamp(seg: SpeechSegment): string {
  return seg.createdAt ?? seg.id;
}

function sortSegments(list: SpeechSegment[], order: SortOrder, threshold: number): SpeechSegment[] {
  const sorted = [...list];
  switch (order) {
    case 'newest':
      return sorted.sort((a, b) => segmentTimestamp(b).localeCompare(segmentTimestamp(a)));
    case 'oldest':
      return sorted.sort((a, b) => segmentTimestamp(a).localeCompare(segmentTimestamp(b)));
    case 'duration':
      return sorted.sort((a, b) => b.durationMs - a.durationMs);
    case 'verified':
      return sorted.sort((a, b) => Number(b.verified) - Number(a.verified));
    case 'confidence':
      return sorted.sort((a, b) => {
        const confA = a.confidence ?? 1.0;
        const confB = b.confidence ?? 1.0;
        return confA - confB;
      });
    case 'activeLearning':
      return sorted.sort((a, b) => {
        const confA = a.confidence ?? 0.5;
        const ctcA = a.ctcScore ?? -5.0;
        const scoreA = Math.max(0.0, 1.0 - confA + 0.1 * -ctcA);

        const confB = b.confidence ?? 0.5;
        const ctcB = b.ctcScore ?? -5.0;
        const scoreB = Math.max(0.0, 1.0 - confB + 0.1 * -ctcB);

        const distA = Math.abs(scoreA - threshold);
        const distB = Math.abs(scoreB - threshold);
        return distA - distB;
      });
    default:
      return sorted;
  }
}

export const selectedSegment = derived(
  [segments, selectedSegmentId],
  ([$segments, $selectedSegmentId]) => $segments.find((s) => s.id === $selectedSegmentId) ?? null,
);

/** The one search predicate, shared by every search-scoped view (curate filter + review queue). */
function applySearchScope(
  segments: SpeechSegment[],
  query: string,
  searchResults: SpeechSegment[] | null,
): SpeechSegment[] {
  if (!query) return segments;
  if (searchResults !== null) {
    const ids = new Set(searchResults.map((s) => s.id));
    return segments.filter((s) => ids.has(s.id));
  }
  const q = query.toLowerCase();
  return segments.filter(
    (s) =>
      s.audioPath?.toLowerCase().includes(q) ||
      (s.rawTranscript?.toLowerCase() ?? '').includes(q) ||
      (s.normalizedTranscript?.toLowerCase() ?? '').includes(q) ||
      (s.annotatedTranscript?.toLowerCase() ?? '').includes(q) ||
      (s.speakerId?.toLowerCase() ?? '').includes(q),
  );
}

export const filteredSegments = derived(
  [segments, filterVerified, searchQuery, searchResults, sortOrder, conformalThreshold],
  ([$segments, $filterVerified, $searchQuery, $searchResults, $sortOrder, $conformalThreshold]) => {
    let result = $segments;
    if ($filterVerified !== null) {
      result = result.filter((s) => s.verified === $filterVerified);
    }
    result = applySearchScope(result, $searchQuery, $searchResults);
    return sortSegments(result, $sortOrder, $conformalThreshold);
  },
);

/**
 * Search scope ONLY — no verified-filter, no sort. This is the review queue's documented contract:
 * deriving it from `filteredSegments` (which applies `filterVerified` FIRST) made the "✓ Verified"
 * chip + any search yield a false "All clips reviewed" while unverified matches existed (true-10
 * audit). Review mode owns its own pending-first ordering, so sorting is deliberately absent too.
 */
export const searchScopedSegments = derived(
  [segments, searchQuery, searchResults],
  ([$segments, $searchQuery, $searchResults]) =>
    applySearchScope($segments, $searchQuery, $searchResults),
);

export const segmentStats = writable({
  total: 0,
  verified: 0,
  pending: 0,
  withAnnotations: 0,
  totalDurationMs: 0,
});

export async function refreshSegmentStats() {
  try {
    const stats = await api.getDatasetStats();
    if (!stats || !Number.isFinite(stats.totalSegments)) return;
    segmentStats.set({
      total: stats.totalSegments,
      verified: stats.verifiedCount,
      pending: stats.pendingCount,
      withAnnotations: 0,
      totalDurationMs: Math.round(stats.totalDurationSeconds * 1000),
    });
  } catch (error) {
    console.error('Failed to load corpus statistics', error);
  }
}
