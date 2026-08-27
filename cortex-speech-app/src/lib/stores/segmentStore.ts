import { writable, derived, get } from 'svelte/store';
import type { SpeechSegment, WordTimestamp } from '../types';
import * as api from '../commands';
import { dedupeById } from '../dedupeById';
import { notifications } from './notificationStore';
import { t } from '../i18n';
import { formatPublicErrorReference } from '../errorText';

// A bounded render window. More rows are appended only as the virtual list approaches its end.
const PAGE_SIZE = 200;
const MAX_RESIDENT_PAGES = 3;
const MAX_RESIDENT_SEGMENTS = PAGE_SIZE * MAX_RESIDENT_PAGES;

function createSegmentsStore() {
  const { subscribe, set, update } = writable<SpeechSegment[]>([]);
  let loadSeq = 0;
  let nextCursor: string | null = null;
  let loadingMore = false;
  return {
    subscribe,
    set,
    update,
    bumpLoadGeneration() {
      loadSeq++;
    },
    async load() {
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
        if (seq !== loadSeq) return;
        set(dedupeById(page.items));
        // Search is part of the server-side page scope now. Never retain a legacy frozen result set
        // across a reload, because it can hide newly matching rows or retain stale matches.
        searchResults.set(null);
        nextCursor = page.nextCursor;
        libraryTotal.set(page.total);
        libraryTruncated.set(nextCursor !== null);
        // Refresh corpus-wide metadata independently of the bounded list window.
        await Promise.all([refreshConformalThreshold(), refreshSegmentStats()]);
        // A newer load (or a write) may have superseded this run DURING the awaited threshold refresh —
        // mirror the in-loop seq guard so a stale/older run cannot CLEAR a newer FAILED load's error
        // (which would silently drop the failure back to a "looks fine" state — the F1 bug, in a race).
        if (seq !== loadSeq) return;
        // The load fully succeeded — clear any prior error state so the view leaves the error branch.
        libraryLoadError.set(null);
      } catch (e) {
        console.error('Failed to load segments', e);
        // P2.1 (audit F1): a DB/IPC read failure must NEVER render as an empty library. The store keeps
        // its prior value (empty on FIRST load), so without this the empty-state showed "No segments
        // loaded" — indistinguishable from a wiped library, in an app whose one law is honesty about
        // data. Surface a distinct, PERSISTENT error the empty view reads (with a Retry), plus a toast
        // for the case where a background reload fails while the user is on another view.
        const msg = formatPublicErrorReference(e) ?? get(t)('errors.unknown');
        libraryLoadError.set(msg);
        notifications.error(get(t)('notifications.loadSegmentsFailed'), { cause: e });
      }
    },
    async loadMore() {
      if (loadingMore || !nextCursor) return;
      const seq = loadSeq;
      const cursor = nextCursor;
      const sort = get(sortOrder);
      const verified = get(filterVerified);
      const query = get(searchQuery).trim() || null;
      loadingMore = true;
      try {
        const page = await api.getSegmentsPage({ verified, query, sort, limit: PAGE_SIZE, cursor });
        if (seq !== loadSeq) return;
        update((current) => {
          const merged = dedupeById([...current, ...page.items]);
          return merged.length > MAX_RESIDENT_SEGMENTS
            ? merged.slice(merged.length - MAX_RESIDENT_SEGMENTS)
            : merged;
        });
        nextCursor = page.nextCursor;
        libraryTotal.set(page.total);
        libraryTruncated.set(nextCursor !== null);
      } catch (e) {
        if (seq !== loadSeq) return;
        notifications.error(get(t)('notifications.loadSegmentsFailed'), { cause: e });
      } finally {
        if (seq === loadSeq) loadingMore = false;
      }
    },
    async hydrate(segmentId: string): Promise<SpeechSegment> {
      const seq = loadSeq;
      const hydrated = await api.getSegment(segmentId);
      // Same last-call-wins guard the page loads use: a slow getSegment resolving AFTER a newer load
      // would revert that row to the pre-reload snapshot, and the next Save-speaker then persists the
      // reverted value. The caller still gets the row it asked for; only the store write is dropped.
      if (seq !== loadSeq) return hydrated;
      update((current) => current.map((row) => (row.id === segmentId ? hydrated : row)));
      return hydrated;
    },
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
