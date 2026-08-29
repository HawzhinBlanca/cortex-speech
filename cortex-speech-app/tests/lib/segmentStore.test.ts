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
  refreshConformalThreshold,
  refreshSegmentStats,
  conformalThreshold,
} from '../../src/lib/stores/segmentStore';
import type { SpeechSegment } from '../../src/lib/types';
import { locale } from '../../src/lib/i18n';
import { notifications } from '../../src/lib/stores/notificationStore';

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

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
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
    notifications.clear();
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

  it('orders duration, verification, confidence, and active-learning scopes with missing scores', () => {
    segments.set([
      makeSeg('a', { durationMs: 100, verified: false, confidence: null, ctcScore: null }),
      makeSeg('b', { durationMs: 300, verified: true, confidence: 0.2, ctcScore: -2 }),
      makeSeg('c', { durationMs: 200, verified: false, confidence: 0.8, ctcScore: -8 }),
    ]);

    sortOrder.set('duration');
    expect(get(filteredSegments).map((row) => row.id)).toEqual(['b', 'c', 'a']);
    sortOrder.set('verified');
    expect(get(filteredSegments)[0].id).toBe('b');
    sortOrder.set('confidence');
    expect(get(filteredSegments).map((row) => row.id)).toEqual(['b', 'c', 'a']);
    conformalThreshold.set(0.5);
    sortOrder.set('activeLearning');
    expect(get(filteredSegments)).toHaveLength(3);
  });

  it('searches every indexed transcript/identity field and honors authoritative search results', () => {
    const rows = [
      makeSeg('audio', { audioPath: 'C:\\voices\\needle.wav' }),
      makeSeg('normalized', { normalizedTranscript: 'normalized needle' }),
      makeSeg('annotated', { annotatedTranscript: 'annotated needle' }),
      makeSeg('speaker', { speakerId: 'needle-speaker' }),
      makeSeg('other', { rawTranscript: 'nothing here' }),
    ];
    segments.set(rows);
    searchQuery.set('needle');
    expect(get(filteredSegments).map((row) => row.id)).toEqual([
      'speaker',
      'normalized',
      'audio',
      'annotated',
    ]);

    searchResults.set([rows[4]]);
    expect(get(filteredSegments).map((row) => row.id)).toEqual(['other']);
  });

  it('keeps invalid optional metadata from poisoning threshold or corpus statistics', async () => {
    conformalThreshold.set(0.42);
    segmentStats.set({ total: 9, verified: 8, pending: 1, withAnnotations: 0, totalDurationMs: 9 });
    invokeMock.mockImplementation(((command: string) => {
      if (command === 'get_dataset_certificate') return Promise.resolve({ threshold: Number.NaN });
      if (command === 'get_dataset_stats') return Promise.resolve({ totalSegments: Number.NaN });
      return Promise.reject(new Error(`unexpected ${command}`));
    }) as typeof invoke);

    await refreshConformalThreshold();
    await refreshSegmentStats();
    expect(get(conformalThreshold)).toBe(0.42);
    expect(get(segmentStats).total).toBe(9);
  });

  it('keeps corpus stats independent from the bounded row window', async () => {
    invokeMock.mockImplementation(fakeBackend(10_000) as typeof invoke);
    await segments.load();
    const stats = get(segmentStats);
    expect(get(segments)).toHaveLength(200);
    expect(stats.total).toBe(10_000);
    expect(stats.totalDurationMs).toBe(10_000_000);
  });

  describe('authoritative row hydration', () => {
    it('waits behind an active full reload, coalesces one row request, and preserves an exact reload receipt', async () => {
      const page = deferred<{
        items: SpeechSegment[];
        total: number;
        nextCursor: string | null;
      }>();
      const hydrated = deferred<SpeechSegment>();
      invokeMock.mockImplementation(((command: string) => {
        if (command === 'get_segments_page') return page.promise;
        if (command === 'get_segment') return hydrated.promise;
        if (command === 'get_dataset_certificate') return Promise.resolve({ threshold: 0.35 });
        if (command === 'get_dataset_stats') {
          return Promise.resolve({
            totalSegments: 1,
            verifiedCount: 0,
            pendingCount: 1,
            totalDurationSeconds: 1,
          });
        }
        return Promise.reject(new Error(`unexpected ${command}`));
      }) as typeof invoke);

      let reloadSettled = false;
      const reload = segments.reloadProjection();
      void reload.then(() => {
        reloadSettled = true;
      });
      const firstHydration = segments.hydrate('a');
      const duplicateHydration = segments.hydrate('a');

      expect(duplicateHydration).toBe(firstHydration);
      expect(invokeMock.mock.calls.filter((call) => call[0] === 'get_segment')).toHaveLength(0);

      page.resolve({ items: [makeSeg('a', { speakerId: 'page' })], total: 1, nextCursor: null });
      await vi.waitFor(() =>
        expect(invokeMock.mock.calls.filter((call) => call[0] === 'get_segment')).toHaveLength(1),
      );
      expect(reloadSettled).toBe(false);

      hydrated.resolve(makeSeg('a', { speakerId: 'hydrated', confidence: 0.91 }));
      await expect(firstHydration).resolves.toMatchObject({
        id: 'a',
        speakerId: 'hydrated',
        confidence: 0.91,
      });
      await expect(duplicateHydration).resolves.toMatchObject({ speakerId: 'hydrated' });
      const receipt = await reload;

      expect(receipt).not.toBeNull();
      expect(segments.projectionReceipt()).toBe(receipt);
      expect(get(segments)).toEqual([
        expect.objectContaining({ id: 'a', speakerId: 'hydrated', confidence: 0.91 }),
      ]);
      expect(invokeMock.mock.calls.filter((call) => call[0] === 'get_segment')).toHaveLength(1);
    });

    it('retires a pre-begin hydration behind an older load without touching the newer receipt', async () => {
      const loadOnePage = deferred<{
        items: SpeechSegment[];
        total: number;
        nextCursor: string | null;
      }>();
      const loadTwoPage = deferred<{
        items: SpeechSegment[];
        total: number;
        nextCursor: string | null;
      }>();
      let pageCall = 0;
      invokeMock.mockImplementation(((command: string) => {
        if (command === 'get_segments_page') {
          pageCall += 1;
          return pageCall === 1 ? loadOnePage.promise : loadTwoPage.promise;
        }
        if (command === 'get_segment') {
          return Promise.reject(new Error('retired hydration must never reach get_segment'));
        }
        if (command === 'get_dataset_certificate') return Promise.resolve({ threshold: 0.35 });
        if (command === 'get_dataset_stats') {
          return Promise.resolve({
            totalSegments: 1,
            verifiedCount: 0,
            pendingCount: 1,
            totalDurationSeconds: 1,
          });
        }
        return Promise.reject(new Error(`unexpected ${command}`));
      }) as typeof invoke);

      const loadOne = segments.reloadProjection();
      await vi.waitFor(() =>
        expect(
          invokeMock.mock.calls.filter((call) => call[0] === 'get_segments_page'),
        ).toHaveLength(1),
      );
      const hydrationOne = segments.hydrate('a');
      await Promise.resolve();

      const loadTwo = segments.reloadProjection();
      await vi.waitFor(() =>
        expect(
          invokeMock.mock.calls.filter((call) => call[0] === 'get_segments_page'),
        ).toHaveLength(2),
      );
      loadTwoPage.resolve({
        items: [makeSeg('a', { speakerId: 'load-two-authority', confidence: 0.97 })],
        total: 1,
        nextCursor: null,
      });
      const receiptTwo = await loadTwo;
      expect(receiptTwo).not.toBeNull();
      expect(segments.projectionReceipt()).toBe(receiptTwo);
      expect(invokeMock.mock.calls.filter((call) => call[0] === 'get_segment')).toHaveLength(0);

      loadOnePage.resolve({
        items: [makeSeg('a', { speakerId: 'retired-load-one', confidence: 0.1 })],
        total: 1,
        nextCursor: null,
      });
      await expect(loadOne).resolves.toBeNull();
      await expect(hydrationOne).resolves.toMatchObject({
        id: 'a',
        speakerId: 'load-two-authority',
        confidence: 0.97,
      });

      expect(invokeMock.mock.calls.filter((call) => call[0] === 'get_segment')).toHaveLength(0);
      expect(get(segments)).toEqual([
        expect.objectContaining({
          id: 'a',
          speakerId: 'load-two-authority',
          confidence: 0.97,
        }),
      ]);
      expect(segments.projectionReceipt()).toBe(receiptTwo);
    });

    it('returns the newer page row when a late older hydration is superseded', async () => {
      const staleHydration = deferred<SpeechSegment>();
      invokeMock.mockImplementation(((command: string) => {
        if (command === 'get_segment') return staleHydration.promise;
        if (command === 'get_segments_page') {
          return Promise.resolve({
            items: [makeSeg('a', { speakerId: 'fresh-page', confidence: 0.95 })],
            total: 1,
            nextCursor: null,
          });
        }
        if (command === 'get_dataset_certificate') return Promise.resolve({ threshold: 0.35 });
        if (command === 'get_dataset_stats') {
          return Promise.resolve({
            totalSegments: 1,
            verifiedCount: 0,
            pendingCount: 1,
            totalDurationSeconds: 1,
          });
        }
        return Promise.reject(new Error(`unexpected ${command}`));
      }) as typeof invoke);
      segments.set([makeSeg('a', { speakerId: 'old-page' })]);

      const hydration = segments.hydrate('a');
      await vi.waitFor(() =>
        expect(invokeMock.mock.calls.filter((call) => call[0] === 'get_segment')).toHaveLength(1),
      );
      await expect(segments.load()).resolves.toBe(true);
      const pageReceipt = segments.projectionReceipt();
      expect(pageReceipt).not.toBeNull();

      staleHydration.resolve(makeSeg('a', { speakerId: 'stale-hydration', confidence: 0.1 }));
      await expect(hydration).resolves.toMatchObject({
        id: 'a',
        speakerId: 'fresh-page',
        confidence: 0.95,
      });
      expect(get(segments)).toEqual([
        expect.objectContaining({ id: 'a', speakerId: 'fresh-page', confidence: 0.95 }),
      ]);
      expect(segments.projectionReceipt()).toBe(pageReceipt);
    });

    it('contains a stale hydration failure but rejects the current hydration failure', async () => {
      const staleHydration = deferred<SpeechSegment>();
      invokeMock.mockImplementation(((command: string) => {
        if (command === 'get_segment') return staleHydration.promise;
        if (command === 'get_segments_page') {
          return Promise.resolve({
            items: [makeSeg('a', { speakerId: 'fresh-page' })],
            total: 1,
            nextCursor: null,
          });
        }
        if (command === 'get_dataset_certificate') return Promise.resolve({ threshold: 0.35 });
        if (command === 'get_dataset_stats') {
          return Promise.resolve({
            totalSegments: 1,
            verifiedCount: 0,
            pendingCount: 1,
            totalDurationSeconds: 1,
          });
        }
        return Promise.reject(new Error(`unexpected ${command}`));
      }) as typeof invoke);
      segments.set([makeSeg('a', { speakerId: 'old-page' })]);

      const superseded = segments.hydrate('a');
      await vi.waitFor(() =>
        expect(invokeMock.mock.calls.filter((call) => call[0] === 'get_segment')).toHaveLength(1),
      );
      await expect(segments.load()).resolves.toBe(true);
      const pageReceipt = segments.projectionReceipt();
      staleHydration.reject(new Error('private stale hydrate failure'));

      await expect(superseded).resolves.toMatchObject({ id: 'a', speakerId: 'fresh-page' });
      expect(segments.projectionReceipt()).toBe(pageReceipt);

      const currentFailure = new Error('current hydrate failed');
      segments.set([makeSeg('b')]);
      invokeMock.mockImplementation(((command: string) => {
        if (command === 'get_segment') return Promise.reject(currentFailure);
        return Promise.reject(new Error(`unexpected ${command}`));
      }) as typeof invoke);

      await expect(segments.hydrate('b')).rejects.toBe(currentFailure);
      expect(get(segments)).toEqual([expect.objectContaining({ id: 'b' })]);
      expect(segments.projectionReceipt()).toBeNull();
    });

    it('keeps a current-row hydration live across additive pagination and certifies the combined projection', async () => {
      const hydratedA = deferred<SpeechSegment>();
      let pageCall = 0;
      invokeMock.mockImplementation(((command: string, args?: unknown) => {
        if (command === 'get_segments_page') {
          pageCall += 1;
          return Promise.resolve(
            pageCall === 1
              ? {
                  items: [makeSeg('a', { speakerId: 'bounded-page' })],
                  total: 2,
                  nextCursor: 'next-page',
                }
              : {
                  items: [makeSeg('b', { speakerId: 'appended-page' })],
                  total: 2,
                  nextCursor: null,
                },
          );
        }
        if (command === 'get_segment') {
          expect(args).toEqual({ segmentId: 'a' });
          return hydratedA.promise;
        }
        if (command === 'get_dataset_certificate') return Promise.resolve({ threshold: 0.35 });
        if (command === 'get_dataset_stats') {
          return Promise.resolve({
            totalSegments: 2,
            verifiedCount: 0,
            pendingCount: 2,
            totalDurationSeconds: 2,
          });
        }
        return Promise.reject(new Error(`unexpected ${command}`));
      }) as typeof invoke);

      await expect(segments.load()).resolves.toBe(true);
      const hydration = segments.hydrate('a');
      await vi.waitFor(() =>
        expect(invokeMock.mock.calls.filter((call) => call[0] === 'get_segment')).toHaveLength(1),
      );
      expect(segments.projectionReceipt()).toBeNull();

      await segments.loadMore();
      expect(get(segments).map((row) => row.id)).toEqual(['a', 'b']);
      expect(segments.projectionReceipt()).toBeNull();

      hydratedA.resolve(makeSeg('a', { speakerId: 'hydrated-authority', confidence: 0.99 }));
      await expect(hydration).resolves.toMatchObject({
        id: 'a',
        speakerId: 'hydrated-authority',
        confidence: 0.99,
      });
      await Promise.resolve();

      expect(get(segments)).toEqual([
        expect.objectContaining({ id: 'a', speakerId: 'hydrated-authority', confidence: 0.99 }),
        expect.objectContaining({ id: 'b', speakerId: 'appended-page' }),
      ]);
      expect(segments.isHydrated('a')).toBe(true);
      expect(segments.projectionReceipt()).not.toBeNull();
    });

    it('fails closed when get_segment returns a different row identity', async () => {
      const original = makeSeg('a', { speakerId: 'page-authority', confidence: 0.8 });
      segments.set([original]);
      invokeMock.mockImplementation(((command: string, args?: unknown) => {
        if (command === 'get_segment') {
          expect(args).toEqual({ segmentId: 'a' });
          return Promise.resolve(makeSeg('wrong-id', { speakerId: 'wrong-authority' }));
        }
        return Promise.reject(new Error(`unexpected ${command}`));
      }) as typeof invoke);

      await expect(segments.hydrate('a')).rejects.toThrow(
        'get_segment returned an invalid payload',
      );

      expect(get(segments)).toEqual([original]);
      expect(segments.isHydrated('a')).toBe(false);
      expect(segments.isHydrated('wrong-id')).toBe(false);
      expect(segments.projectionReceipt()).toBeNull();
    });

    it('times out a wedged row, lets the next selection become actionable, and ignores the late row', async () => {
      vi.useFakeTimers();
      try {
        const nativeA = deferred<SpeechSegment>();
        invokeMock.mockImplementation(((command: string, args?: unknown) => {
          if (command === 'get_segment') {
            const { segmentId } = args as { segmentId: string };
            if (segmentId === 'a') return nativeA.promise;
            if (segmentId === 'b') {
              return Promise.resolve(
                makeSeg('b', { speakerId: 'current-selection', confidence: 0.98 }),
              );
            }
          }
          return Promise.reject(new Error(`unexpected ${command}`));
        }) as typeof invoke);
        segments.set([
          makeSeg('a', { speakerId: 'page-a' }),
          makeSeg('b', { speakerId: 'page-b' }),
        ]);

        const hydrationA = segments.hydrate('a');
        await vi.advanceTimersByTimeAsync(0);
        expect(invokeMock).toHaveBeenCalledWith('get_segment', { segmentId: 'a' });
        expect(segments.projectionReceipt()).toBeNull();

        await vi.advanceTimersByTimeAsync(15_000);
        await expect(hydrationA).rejects.toThrow('E_SEGMENT_LOAD_TIMEOUT');

        selectedSegmentId.set('b');
        await expect(segments.hydrate('b')).resolves.toMatchObject({
          id: 'b',
          speakerId: 'current-selection',
          confidence: 0.98,
        });
        await vi.advanceTimersByTimeAsync(0);
        const receiptB = segments.projectionReceipt();
        expect(receiptB).not.toBeNull();
        expect(get(selectedSegment)?.id).toBe('b');
        expect(segments.isHydrated('b')).toBe(true);

        nativeA.resolve(makeSeg('a', { speakerId: 'late-stale-row', confidence: 0.01 }));
        await vi.advanceTimersByTimeAsync(0);

        expect(get(segments)).toEqual([
          expect.objectContaining({ id: 'a', speakerId: 'page-a' }),
          expect.objectContaining({
            id: 'b',
            speakerId: 'current-selection',
            confidence: 0.98,
          }),
        ]);
        expect(segments.isHydrated('a')).toBe(false);
        expect(segments.projectionReceipt()).toBe(receiptB);
      } finally {
        vi.useRealTimers();
      }
    });
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
      await segments.loadMore();
      expect(invokeMock.mock.calls.filter((c) => c[0] === 'get_segments_page')).toHaveLength(1);
    });

    it('deduplicates concurrent next-page requests and contains a current-page failure', async () => {
      invokeMock.mockImplementation(fakeBackend(400) as typeof invoke);
      await segments.load();
      let rejectPage!: (error: unknown) => void;
      invokeMock.mockImplementation(((command: string) => {
        if (command === 'get_segments_page') {
          return new Promise((_resolve, reject) => {
            rejectPage = reject;
          });
        }
        return Promise.resolve({ threshold: 0.35 });
      }) as typeof invoke);

      const first = segments.loadMore();
      const duplicate = segments.loadMore();
      expect(invokeMock.mock.calls.filter((call) => call[0] === 'get_segments_page')).toHaveLength(
        2,
      );
      await duplicate;
      rejectPage(new Error('page unavailable'));
      await first;
      expect(get(notifications).at(-1)?.type).toBe('error');
      expect(get(segments)).toHaveLength(200);
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

    it('drops a stale load failure after a newer import generation takes authority', async () => {
      let rejectLoad!: (error: unknown) => void;
      invokeMock.mockImplementation(((command: string) => {
        if (command === 'get_segments_page') {
          return new Promise((_resolve, reject) => {
            rejectLoad = reject;
          });
        }
        return Promise.resolve({ threshold: 0.35 });
      }) as typeof invoke);

      const staleLoad = segments.load();
      segments.bumpLoadGeneration();
      rejectLoad(new Error('private stale failure'));
      await staleLoad;

      expect(get(libraryLoadError)).toBeNull();
      expect(get(notifications)).toEqual([]);
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
