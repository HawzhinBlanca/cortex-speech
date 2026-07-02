import { writable, derived } from 'svelte/store';
import type { SpeechSegment, WordTimestamp } from '../types';
import * as api from '../commands';
import { isHumanRejected } from '../segmentQuality';

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
      try {
        const data = await api.getSegments();
        if (seq !== loadSeq) return; // stale load — a newer one is in flight or a write invalidated it
        set(data);
        // Refresh threshold after loading segments
        await refreshConformalThreshold();
      } catch (e) {
        console.error('Failed to load segments', e);
      }
    },
  };
}

export const segments = createSegmentsStore();
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

export const filteredSegments = derived(
  [segments, filterVerified, searchQuery, searchResults, sortOrder, conformalThreshold],
  ([$segments, $filterVerified, $searchQuery, $searchResults, $sortOrder, $conformalThreshold]) => {
    let result = $segments;
    if ($filterVerified !== null) {
      result = result.filter((s) => s.verified === $filterVerified);
    }
    if ($searchQuery) {
      if ($searchResults !== null) {
        const ids = new Set($searchResults.map((s) => s.id));
        result = result.filter((s) => ids.has(s.id));
      } else {
        const q = $searchQuery.toLowerCase();
        result = result.filter(
          (s) =>
            s.audioPath?.toLowerCase().includes(q) ||
            (s.rawTranscript?.toLowerCase() ?? '').includes(q) ||
            (s.normalizedTranscript?.toLowerCase() ?? '').includes(q) ||
            (s.annotatedTranscript?.toLowerCase() ?? '').includes(q) ||
            (s.speakerId?.toLowerCase() ?? '').includes(q),
        );
      }
    }
    return sortSegments(result, $sortOrder, $conformalThreshold);
  },
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
