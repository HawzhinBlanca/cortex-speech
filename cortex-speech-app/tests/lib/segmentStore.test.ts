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
import { locale } from '../../src/lib/i18n';

const invokeMock = vi.mocked(invoke);

// A fake of the Rust get_segments_page backend: offset cursor, honest total, next_cursor null at end.
// Items are generated per page (no giant pre-built array) so even the MAX_LOAD test stays cheap.
function fakeBackend(total: number) {
  return (command: string, args?: unknown): Promise<unknown> => {
    if (command === 'get_dataset_certificate') return Promise.resolve({ threshold: 0.35 });
    if (command === 'get_dataset_stats')
      return Promise.resolve({
        totalSegments: total,
        verifiedCount: 0,
        pendingCount: total,
        totalDurationSeconds: total,
      });
    if (command !== 'get_segments_page') return Promise.reject(new Error(`unexpected ${command}`));
    const a = args as { limit: number; cursor: string | null };
    const offset = a.cursor ? parseInt(a.cursor, 10) : 0;
    const count = Math.max(0, Math.min(a.limit, total - offset));
    const items = Array.from({ length: count }, (_, k) => makeSeg(String(offset + k)));
    const nextOffset = offset + items.length;
    return Promise.resolve({
      items,
      total,
      nextCursor: nextOffset < total ? String(nextOffset) : null,
    });
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
    locale.set('en');
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
    expect(get(segments).map((s) => s.id)).toEqual(['c', 'a', 'b']);
    expect(original.map((s) => s.id)).toEqual(['c', 'a', 'b']);
  });

  it('keeps corpus stats independent from the bounded row window', async () => {
    invokeMock.mockImplementation(fakeBackend(10_000) as typeof invoke);
    await segments.load();
    const stats = get(segmentStats);
    expect(get(segments)).toHaveLength(200);
    expect(stats.total).toBe(10_000);
    expect(stats.totalDurationMs).toBe(10_000_000);
  });

  describe('windowed keyset pagination', () => {
    it('loads one bounded page, then appends only on demand', async () => {
      invokeMock.mockImplementation(fakeBackend(10_000) as typeof invoke);
      await segments.load();

      expect(get(segments)).toHaveLength(200);
      expect(get(libraryTotal)).toBe(10_000);
      expect(get(libraryTruncated)).toBe(true);

      let pageCalls = invokeMock.mock.calls.filter((c) => c[0] === 'get_segments_page');
      expect(pageCalls).toHaveLength(1);
      expect((pageCalls[0][1] as { cursor: string | null }).cursor).toBeNull();

      await segments.loadMore();
      expect(get(segments)).toHaveLength(400);
      pageCalls = invokeMock.mock.calls.filter((c) => c[0] === 'get_segments_page');
      expect(pageCalls).toHaveLength(2);
      expect((pageCalls[1][1] as { cursor: string | null }).cursor).toBe('200');
    });

    it('retains at most three pages while continuing the forward keyset walk', async () => {
      invokeMock.mockImplementation(fakeBackend(1_000) as typeof invoke);
      await segments.load();
      await segments.loadMore();
      await segments.loadMore();
      expect(get(segments)).toHaveLength(600);

      await segments.loadMore();
      const resident = get(segments);
      expect(resident).toHaveLength(600);
      expect(resident[0].id).toBe('200');
      expect(resident.at(-1)?.id).toBe('799');
      expect(get(libraryTotal)).toBe(1_000);
      expect(get(libraryTruncated)).toBe(true);
    });

    it('loads exactly the last partial page and stops', async () => {
      invokeMock.mockImplementation(fakeBackend(11) as typeof invoke);
      await segments.load();
      expect(get(segments)).toHaveLength(11);
      expect(get(libraryTruncated)).toBe(false);
      expect(invokeMock.mock.calls.filter((c) => c[0] === 'get_segments_page')).toHaveLength(1);
    });

    it('does not eagerly allocate a very large library', async () => {
      invokeMock.mockImplementation(fakeBackend(50_001) as typeof invoke);
      await segments.load();
      expect(get(segments)).toHaveLength(200);
      expect(get(libraryTotal)).toBe(50_001);
      expect(get(libraryTruncated)).toBe(true);
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
      // library. load() must set libraryLoadError (localized public text) instead of swallowing it, so
      // the empty view renders "load failed + Retry". A subsequent successful load must clear it.
      invokeMock.mockImplementation(((command: string) => {
        if (command === 'get_dataset_certificate') return Promise.resolve({ threshold: 0.35 });
        return Promise.reject(new Error('database is locked'));
      }) as typeof invoke);
      await segments.load();
      expect(get(libraryLoadError)).toContain('unexpected error');
      expect(get(libraryLoadError)).not.toContain('database is locked');
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

    it('drops a hydrate response that a newer load superseded', async () => {
      // hydrate() writes the fetched row back into the store AFTER an await. A slow get_segment
      // resolving once a fresher load() has already replaced that row reverted it to the pre-reload
      // snapshot — and the next Save-speaker then persists the reverted value. Same last-call-wins
      // guard load()/loadMore() use. Fail-before: the store row reads 'stale' here.
      let releaseHydrate!: () => void;
      invokeMock.mockImplementation(((command: string) => {
        if (command === 'get_dataset_certificate') return Promise.resolve({ threshold: 0.35 });
        if (command === 'get_dataset_stats')
          return Promise.resolve({
            totalSegments: 1,
            verifiedCount: 0,
            pendingCount: 1,
            totalDurationSeconds: 1,
          });
        if (command === 'get_segment')
          return new Promise<SpeechSegment>((resolve) => {
            releaseHydrate = () => resolve(makeSeg('a', { speakerId: 'stale' }));
          });
        if (command === 'get_segments_page')
          return Promise.resolve({
            items: [makeSeg('a', { speakerId: 'fresh' })],
            total: 1,
            nextCursor: null,
          });
        return Promise.reject(new Error(`unexpected ${command}`));
      }) as typeof invoke);

      const hydrating = segments.hydrate('a');
      await segments.load(); // a newer load replaces the row while the hydrate is in flight
      releaseHydrate();

      // The caller still receives the row it asked for; only the store WRITE is dropped.
      await expect(hydrating).resolves.toMatchObject({ speakerId: 'stale' });
      expect(get(segments)[0].speakerId).toBe('fresh');
    });

    it('binds the active view scope into every page request', async () => {
      filterVerified.set(true);
      searchQuery.set('hello');
      invokeMock.mockImplementation(fakeBackend(3) as typeof invoke);
      await segments.load();

      const pageCalls = invokeMock.mock.calls.filter((c) => c[0] === 'get_segments_page');
      const args = pageCalls[0][1] as { verified: boolean | null; query: string | null };
      expect(args.verified).toBe(true);
      expect(args.query).toBe('hello');
    });
  });
});
