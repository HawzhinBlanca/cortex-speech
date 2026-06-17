import { describe, it, expect, beforeEach } from 'vitest';
import { get } from 'svelte/store';
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
} from '../../src/lib/stores/segmentStore';
import type { SpeechSegment } from '../../src/lib/types';

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
});
