import { describe, it, expect } from 'vitest';
import {
  parseSourceMeta,
  parseWordTimestamps,
  mergeWordTimestamps,
  segmentChunkLabel,
  truncateFilename,
  segmentSourceFilename,
} from '../../src/lib/alignment';
import type { WordTimestamp } from '../../src/lib/types';
import { isModelError, parseActionableError } from '../../src/lib/errors';

const sampleWords: WordTimestamp[] = [
  { word: 'hello', start: 0, end: 0.4, confidence: 0.95 },
  { word: 'world', start: 0.4, end: 0.9, confidence: 0.88 },
];

describe('alignment helpers', () => {
  it('truncates long filenames while keeping extension', () => {
    const truncated = truncateFilename('very-long-recording-name.wav', 18);
    expect(truncated.length).toBeLessThanOrEqual(18);
    expect(truncated).toContain('.wav');
    expect(truncated).toContain('…');
  });

  it('returns chunk label from alignment metadata', () => {
    const json = JSON.stringify({
      source_start_ms: 0,
      source_end_ms: 5000,
      chunk_index: 2,
      chunk_count: 12,
    });
    expect(segmentChunkLabel(json)).toBe('3/12');
    expect(segmentChunkLabel(null)).toBeNull();
  });

  it('extracts basename from audio path', () => {
    expect(segmentSourceFilename('C:\\audio\\sample.wav')).toBe('sample.wav');
  });

  it('parses source metadata', () => {
    const meta = parseSourceMeta(JSON.stringify({
      source_start_ms: 1000,
      source_end_ms: 4000,
      chunk_index: 0,
      chunk_count: 2,
    }));
    expect(meta?.sourceStartMs).toBe(1000);
    expect(meta?.chunkCount).toBe(2);
  });

  it('parseSourceMeta returns null for invalid or array JSON', () => {
    expect(parseSourceMeta(null)).toBeNull();
    expect(parseSourceMeta('')).toBeNull();
    expect(parseSourceMeta('not-json')).toBeNull();
    expect(parseSourceMeta(JSON.stringify(sampleWords))).toBeNull();
    expect(parseSourceMeta(JSON.stringify({ source_start_ms: 'x', source_end_ms: 1 }))).toBeNull();
  });

  it('parseWordTimestamps reads array or words property', () => {
    expect(parseWordTimestamps(null)).toEqual([]);
    expect(parseWordTimestamps(JSON.stringify(sampleWords))).toEqual(sampleWords);
    expect(parseWordTimestamps(JSON.stringify({ words: sampleWords }))).toEqual(sampleWords);
    expect(parseWordTimestamps(JSON.stringify({ source_start_ms: 0 }))).toEqual([]);
    expect(parseWordTimestamps('{')).toEqual([]);
  });

  it('fails closed on malformed, non-finite, or out-of-order timing data', () => {
    expect(parseWordTimestamps('[{"word":"x","start":2,"end":1,"confidence":0.9}]')).toEqual([]);
    expect(
      parseWordTimestamps('[{"word":"x","start":0,"end":1,"confidence":2}]'),
    ).toEqual([]);
    expect(
      parseWordTimestamps(
        '[{"word":"a","start":1,"end":2,"confidence":0.9},{"word":"b","start":0,"end":1,"confidence":0.8}]',
      ),
    ).toEqual([]);
    expect(
      parseSourceMeta('{"source_start_ms":5000,"source_end_ms":1000,"chunk_index":2,"chunk_count":2}'),
    ).toBeNull();
  });

  it('mergeWordTimestamps preserves metadata and sets words', () => {
    const base = JSON.stringify({
      source_start_ms: 0,
      source_end_ms: 5000,
      chunk_index: 1,
      chunk_count: 3,
    });
    const merged = JSON.parse(mergeWordTimestamps(base, sampleWords)) as Record<string, unknown>;
    expect(merged.source_start_ms).toBe(0);
    expect(merged.chunk_count).toBe(3);
    expect(merged.words).toEqual(sampleWords);
  });

  it('mergeWordTimestamps creates words-only object when no base', () => {
    const merged = JSON.parse(mergeWordTimestamps(null, sampleWords)) as { words: WordTimestamp[] };
    expect(merged.words).toEqual(sampleWords);
  });

  it('mergeWordTimestamps ignores invalid existing JSON', () => {
    const merged = JSON.parse(mergeWordTimestamps('[1,2]', sampleWords)) as { words: WordTimestamp[] };
    expect(merged.words).toEqual(sampleWords);
  });
});

describe('errors', () => {
  it('detects model-related failures', () => {
    expect(isModelError('Missing models: Meta OmniASR CTC 300M (model)')).toBe(true);
    expect(isModelError('File not found')).toBe(false);
  });

  it('returns actionable model error guidance', () => {
    const parsed = parseActionableError(new Error('Model not found at /models: Missing models: foo'));
    expect(parsed.action).toBeDefined();
    expect(parsed.action?.handler).toBeTypeOf('function');
    expect(parsed.detail).toContain('Missing models');
  });
});
