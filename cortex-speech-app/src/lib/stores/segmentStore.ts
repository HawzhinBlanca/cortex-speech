import { writable, derived, get } from 'svelte/store';
import type { SpeechSegment, WordTimestamp } from '../types';
import * as api from '../commands';
import { isHumanRejected } from '../segmentQuality';

// Fetch the whole library by walking every backend page. Page size is the backend's max (fewest
// round-trips). MAX_LOAD is a defensive ceiling so a pathological library can't exhaust memory or
// hang the load — it is far above any realistic personal review set, and when it IS hit we keep the
// true `total` and flag the view truncated instead of silently hiding rows.
const PAGE_SIZE = 500;
const MAX_LOAD = 50_000;

function createSegmentsStore() {
  const { subscribe, set, update } = writable<SpeechSegment[]>([]);
  let loadSeq = 0;
  return {
    subscribe,
    set,
    update,
    bumpLoadGeneration() {
      loadSeq++;
    },
    async load() {
      const seq = ++loadSeq;
      // Snapshot the filter params ONCE: a mid-load store change must not drift the query across
      // pages. A real change bumps loadSeq elsewhere, and this run bails at the next stale-check.
      const verified = get(filterVerified);
      const query = get(searchQuery) || null;
      const sort = get(sortOrder);
      try {
        const acc: SpeechSegment[] = [];
        let cursor: string | null = null;
        let total = 0;
        // Walk EVERY page. The old code took only the first 300 rows, so any larger library silently
        // dropped everything past 300 from review, lists, and every store-derived stat.
        do {
          const page = await api.getSegmentsPage({ verified, query, sort, limit: PAGE_SIZE, cursor });
          if (seq !== loadSeq) return; // a newer load or a write superseded this run
          total = page.total;
          acc.push(...page.items);
          cursor = page.nextCursor;
          if (page.items.length === 0) break; // defensive: never spin on a non-null cursor + empty page
        } while (cursor && acc.length < MAX_LOAD);
        set(acc);
        libraryTotal.set(total);
        const truncated = acc.length < total;
        libraryTruncated.set(truncated);
        if (truncated) {
          console.warn(
            `Loaded ${acc.length} of ${total} segments (MAX_LOAD=${MAX_LOAD} reached). ` +
              'Narrow with search/filter to review the rest.',
          );
        }
        // Refresh threshold after loading segments
        await refreshConformalThreshold();
      } catch (e) {
        console.error('Failed to load segments', e);
      }
    },
  };
}

export const segments = createSegmentsStore();
// True backend row count for the current filter (may exceed segments.length only when MAX_LOAD is
// hit). `libraryTruncated` = the loaded view is a prefix of a larger library — surface it, never
// hide it. For any realistic personal library these are `segments.length` and `false`.
export const libraryTotal = writable(0);
export const libraryTruncated = writable(false);
export const selectedSegmentId = writable<string | null>(null);
export const wordTimestamps = writable<WordTimestamp[]>([]);
export const filterVerified = writable<boolean | null>(null);
export const searchQuery = writable('');
export const searchResults = writable<SpeechSegment[] | null>(null);
export const searchLoading = writable(false);
export type SortOrder =
  | 'newest'
  | 'oldest'
  | 'duration'
  | 'verified'
  | 'confidence'
  | 'activeLearning';
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
  ([$segments, $searchQuery, $searchResults]) => applySearchScope($segments, $searchQuery, $searchResults),
);

export const segmentStats = derived(segments, ($segments) => {
  let verified = 0,
    pending = 0,
    withAnnotations = 0,
    totalDurationMs = 0;
  for (const s of $segments) {
    // A human-rejected clip ("mark bad") carries verified=true only to leave the review queue — it is
    // neither confirmed-good (verified) nor still-pending, so it counts toward neither bucket.
    if (isHumanRejected(s)) {
      // rejected: excluded from both verified and pending, still part of total
    } else if (s.verified) {
      verified++;
    } else {
      pending++;
    }
    if (s.annotatedTranscript) withAnnotations++;
    totalDurationMs += s.durationMs;
  }
  return { total: $segments.length, verified, pending, withAnnotations, totalDurationMs };
});
