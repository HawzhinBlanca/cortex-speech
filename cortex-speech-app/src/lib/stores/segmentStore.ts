import { writable, derived, get } from 'svelte/store';
import type { SpeechSegment, WordTimestamp } from '../types';
import * as api from '../commands';
import { isHumanRejected, hasRealTranscript } from '../segmentQuality';
import { dedupeById } from '../dedupeById';
import { notifications } from './notificationStore';
import { t } from '../i18n';

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
      // ALWAYS load the whole library — never bake the view filters (verified chip / search query)
      // into the backend query. Every consumer treats this store AS the whole library and applies
      // the filters client-side (filteredSegments, searchScopedSegments, segmentStats, export's
      // verified filter), and background reloads fire while filters are active (batch/import
      // completion, wsl-status, ReviewMode.ensureWordTimings) — a filtered reload silently
      // replaced the library with a stale subset that every consumer then mis-derived from.
      const sort = get(sortOrder);
      try {
        const acc: SpeechSegment[] = [];
        let cursor: string | null = null;
        let total = 0;
        // Walk EVERY page. The old code took only the first 300 rows, so any larger library silently
        // dropped everything past 300 from review, lists, and every store-derived stat.
        do {
          const page = await api.getSegmentsPage({ verified: null, query: null, sort, limit: PAGE_SIZE, cursor });
          if (seq !== loadSeq) return; // a newer load or a write superseded this run
          total = page.total;
          acc.push(...page.items);
          cursor = page.nextCursor;
          if (page.items.length === 0) break; // defensive: never spin on a non-null cursor + empty page
        } while (cursor && acc.length < MAX_LOAD);
        // Cross-page dedupe: the backend "cursor" is a plain OFFSET, so a concurrent insert (e.g. the
        // import worker writing rows between page fetches under the newest-first sort) shifts rows
        // down and the next page re-serves ones already accumulated. Duplicate ids in this store crash
        // keyed {#each} consumers (each_key_duplicate) and double-count stats — dedupe is the fix at
        // the SOURCE (keep-first preserves page order); VirtualList's own dedupe stays as belt-and-braces.
        set(dedupeById(acc));
        // A reload replaces every row, so an ACTIVE FTS search's frozen match-id set (computed against the
        // PRE-reload library) is now stale: applySearchScope would keep filtering the fresh rows by it —
        // hiding clips whose refined transcript newly matches the query and keeping ones that no longer do,
        // in BOTH the curate filter and the review queue, until the user retypes. Drop the snapshot so the
        // scope falls back to applySearchScope's LIVE substring predicate until the next keystroke re-runs
        // FTS (this is the "reloads fire while filters are active" case documented above).
        if (get(searchResults) !== null) searchResults.set(null);
        libraryTotal.set(total);
        const truncated = acc.length < total;
        libraryTruncated.set(truncated);
        if (truncated) {
          // The store always holds an unfiltered prefix, so search/filter chips cannot pull in the
          // dropped tail — say so honestly instead of advising a filter that won't change the load.
          console.warn(
            `Loaded ${acc.length} of ${total} segments (MAX_LOAD=${MAX_LOAD} reached). ` +
              'Rows past the ceiling are not shown; libraryTruncated is set.',
          );
        }
        // Refresh threshold after loading segments
        await refreshConformalThreshold();
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
        const msg = e instanceof Error ? e.message : String(e);
        libraryLoadError.set(msg);
        notifications.error(get(t)('notifications.loadSegmentsFailed'), { detail: msg });
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
    } else if (s.verified && hasRealTranscript(s)) {
      verified++;
    } else if (s.verified) {
      // verified=true but NO real transcript — e.g. a placeholder clip ("[Pending WSL 7B ASR]") caught by
      // "Verify all pending" (batch_verify has no content guard). It can't ship (export_dataset drops every
      // placeholder/empty row via the training grade), and it is not pending-review (already marked verified),
      // so — like a rejected clip — it counts toward NEITHER bucket, keeping the "verified" tally aligned with
      // what the exported dataset can actually contain.
    } else {
      pending++;
    }
    if (s.annotatedTranscript) withAnnotations++;
    totalDurationMs += s.durationMs;
  }
  return { total: $segments.length, verified, pending, withAnnotations, totalDurationMs };
});
