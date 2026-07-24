import { describe, it, expect, beforeEach, vi } from 'vitest';
import { get } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import {
  segments,
  selectedSegmentId,
  filterVerified,
  searchQuery,
  searchResults,
  sortOrder,
  selectedSegment,
  filteredSegments,
  segmentStats,
  libraryTotal,
  libraryTruncated,
  libraryLoadError,
} from '../../src/lib/stores/segmentStore';
import type { SpeechSegment } from '../../src/lib/types';

const invokeMock = vi.mocked(invoke);

// A fake of the Rust get_segments_page backend: offset cursor, honest total, next_cursor null at end.
// Items are generated per page (no giant pre-built array) so even the MAX_LOAD test stays cheap.
function fakeBackend(total: number) {
  return (command: string, args?: unknown): Promise<unknown> => {
    if (command === 'get_dataset_certificate') return Promise.resolve({ threshold: 0.35 });
    if (command !== 'get_segments_page') return Promise.reject(new Error(`unexpected ${command}`));
    const a = args as { limit: number; cursor: string | null };
    const offset = a.cursor ? parseInt(a.cursor, 10) : 0;
    const count = Math.max(0, Math.min(a.limit, total - offset));
    const items = Array.from({ length: count }, (_, k) => makeSeg(String(offset + k)));
    const nextOffset = offset + items.length;
    return Promise.resolve({ items, total, nextCursor: nextOffset < total ? String(nextOffset) : null });
  };
}

function makeSeg(id: string, overrides: Partial<SpeechSegment> = {}): SpeechSegment {
  return {
    id,
    audioPath: `${id}.wav`,
    rawTranscript: 'test',
    normalizedTranscript: null,
    annotatedTranscript: null,
    alignmentJson: null,
    durationMs: 1000,
    speakerId: null,
    verified: false,
    ...overrides,
  };
}

describe('segmentStore', () => {
  beforeEach(() => {
    segments.set([]);
    selectedSegmentId.set(null);
    filterVerified.set(null);
    searchQuery.set('');
    searchResults.set(null);
    sortOrder.set('newest');
    libraryTotal.set(0);
    libraryTruncated.set(false);
    libraryLoadError.set(null);
    invokeMock.mockReset();
  });

  it('starts empty', () => {
    expect(get(segments)).toHaveLength(0);
    expect(get(selectedSegmentId)).toBeNull();
    expect(get(selectedSegment)).toBeNull();
  });

  it('loads segments', () => {
    segments.set([makeSeg('1'), makeSeg('2')]);
    expect(get(segments)).toHaveLength(2);
  });

  it('tracks selected segment', () => {
    const segs = [makeSeg('a', { rawTranscript: 'hello' }), makeSeg('b')];
    segments.set(segs);
    selectedSegmentId.set('a');
    expect(get(selectedSegment)?.id).toBe('a');
    expect(get(selectedSegment)?.rawTranscript).toBe('hello');
  });

  it('filters by verified', () => {
    segments.set([makeSeg('v1', { verified: true }), makeSeg('v2', { verified: false })]);
    filterVerified.set(true);
    const filtered = get(filteredSegments);
    expect(filtered).toHaveLength(1);
    expect(filtered[0].id).toBe('v1');
  });

  it('filters by search query', () => {
    segments.set([
      makeSeg('s1', { rawTranscript: 'hello world' }),
      makeSeg('s2', { rawTranscript: 'goodbye universe' }),
    ]);
    searchQuery.set('hello');
    const filtered = get(filteredSegments);
    expect(filtered).toHaveLength(1);
    expect(filtered[0].id).toBe('s1');
  });

  it('sorts by oldest', () => {
    segments.set([
      makeSeg('c', { durationMs: 500 }),
      makeSeg('a', { durationMs: 2000 }),
      makeSeg('b', { durationMs: 1000 }),
    ]);
    sortOrder.set('oldest');
    const filtered = get(filteredSegments);
    expect(filtered[0].id).toBe('a');
    expect(filtered[1].id).toBe('b');
    expect(filtered[2].id).toBe('c');
  });

  it('sort does not mutate the segments store', () => {
    const original = [
      makeSeg('c', { durationMs: 500 }),
      makeSeg('a', { durationMs: 2000 }),
      makeSeg('b', { durationMs: 1000 }),
    ];
    segments.set(original);
    sortOrder.set('oldest');
    get(filteredSegments);
    expect(get(segments).map(s => s.id)).toEqual(['c', 'a', 'b']);
    expect(original.map(s => s.id)).toEqual(['c', 'a', 'b']);
  });

  it('computes segmentStats', () => {
    segments.set([
      makeSeg('a', { durationMs: 1000, verified: true }),
      makeSeg('b', { durationMs: 2000, verified: false }),
      makeSeg('c', { durationMs: 3000, verified: true, annotatedTranscript: 'annotated' }),
    ]);
    const stats = get(segmentStats);
    expect(stats.total).toBe(3);
    expect(stats.totalDurationMs).toBe(6000);
    expect(stats.verified).toBe(2);
    expect(stats.pending).toBe(1);
    expect(stats.withAnnotations).toBe(1);
  });

  describe('load() pagination — no hidden segments past the old 300 cap', () => {
    it('loads the ENTIRE library across pages (10k fixture), with correct global counts', async () => {
      invokeMock.mockImplementation(fakeBackend(10_000) as typeof invoke);
      await segments.load();

      expect(get(segments)).toHaveLength(10_000); // was capped at 300 before the fix
      expect(get(segmentStats).total).toBe(10_000); // store-derived stats now count the whole library
      expect(get(libraryTotal)).toBe(10_000);
      expect(get(libraryTruncated)).toBe(false);

      const pageCalls = invokeMock.mock.calls.filter((c) => c[0] === 'get_segments_page');
      expect(pageCalls).toHaveLength(20); // 10000 / 500 — loop terminates, no infinite paging
      // First page has no cursor; the second resumes at offset 500 (offset-cursor forwarded correctly).
      expect((pageCalls[0][1] as { cursor: string | null }).cursor).toBeNull();
      expect((pageCalls[1][1] as { cursor: string | null }).cursor).toBe('500');
    });

    it('loads exactly the last partial page and stops (11 rows, page size 500-bounded)', async () => {
      invokeMock.mockImplementation(fakeBackend(11) as typeof invoke);
      await segments.load();
      expect(get(segments)).toHaveLength(11);
      expect(get(libraryTruncated)).toBe(false);
      expect(invokeMock.mock.calls.filter((c) => c[0] === 'get_segments_page')).toHaveLength(1);
    });

    it('caps at MAX_LOAD and FLAGS truncation (keeps the true total) rather than silently hiding rows', async () => {
      invokeMock.mockImplementation(fakeBackend(50_001) as typeof invoke);
      await segments.load();
      expect(get(segments)).toHaveLength(50_000); // MAX_LOAD ceiling
      expect(get(libraryTotal)).toBe(50_001); // the real count is preserved, not lost
      expect(get(libraryTruncated)).toBe(true); // surfaced honestly
    });

    it('an empty library loads cleanly with a zero total', async () => {
      invokeMock.mockImplementation(fakeBackend(0) as typeof invoke);
      await segments.load();
      expect(get(segments)).toHaveLength(0);
      expect(get(libraryTotal)).toBe(0);
      expect(get(libraryTruncated)).toBe(false);
    });

    it('surfaces a load FAILURE as a distinct error state, not an empty library, and clears it on retry', async () => {
      // P2.1 (audit F1): a DB/IPC read failure must NEVER be indistinguishable from an empty/wiped
      // library. load() must set libraryLoadError (the backend message) instead of swallowing it, so
      // the empty view renders "load failed + Retry". A subsequent successful load must clear it.
      invokeMock.mockImplementation(((command: string) => {
        if (command === 'get_dataset_certificate') return Promise.resolve({ threshold: 0.35 });
        return Promise.reject(new Error('database is locked'));
      }) as typeof invoke);
      await segments.load();
      expect(get(libraryLoadError)).toBe('database is locked');
      expect(get(segments)).toHaveLength(0); // still empty, but the view now knows WHY

      // Retry succeeds -> the error state clears so the view leaves the error branch.
      invokeMock.mockImplementation(fakeBackend(3) as typeof invoke);
      await segments.load();
      expect(get(libraryLoadError)).toBeNull();
      expect(get(segments)).toHaveLength(3);
    });

    it('a run superseded during the threshold refresh does NOT clear a newer load error (race guard)', async () => {
      // Adversarial-found race: the success-path libraryLoadError.set(null) runs AFTER the awaited
      // conformal-threshold refresh. Without a seq guard there, an OLDER load resuming after a NEWER
      // load already failed (and set the error) would clear it — silently dropping the failure. Simulate
      // by bumping the load generation DURING this run's threshold await; the guard must then bail before
      // clearing, leaving the newer load's error intact.
      libraryLoadError.set('a newer load already failed');
      invokeMock.mockImplementation(((command: string) => {
        if (command === 'get_dataset_certificate') {
          segments.bumpLoadGeneration(); // a newer load supersedes this one mid-await
          return Promise.resolve({ threshold: 0.35 });
        }
        if (command === 'get_segments_page') {
          return Promise.resolve({ items: [makeSeg('a')], total: 1, nextCursor: null });
        }
        return Promise.reject(new Error(`unexpected ${command}`));
      }) as typeof invoke);
      await segments.load();
      expect(get(libraryLoadError)).toBe('a newer load already failed');
    });

    it('NEVER bakes the view filters into the backend query — the store is always the whole library', async () => {
      // Background reloads (batch/import completion, wsl-status, ReviewMode.ensureWordTimings) fire
      // while the verified chip / search box are active. A filtered server query silently replaced
      // the store with a stale subset that every consumer (filteredSegments, segmentStats, export's
      // verified filter) then treated as the whole library. Filtering is client-side, always.
      filterVerified.set(true);
      searchQuery.set('hello');
      invokeMock.mockImplementation(fakeBackend(3) as typeof invoke);
      await segments.load();

      expect(get(segments)).toHaveLength(3); // the full library, not the filtered subset
      const pageCalls = invokeMock.mock.calls.filter((c) => c[0] === 'get_segments_page');
      expect(pageCalls.length).toBeGreaterThan(0);
      for (const call of pageCalls) {
        const args = call[1] as { verified: boolean | null; query: string | null };
        expect(args.verified).toBeNull();
        expect(args.query).toBeNull();
      }
    });
  });
});
