import { describe, it, expect } from 'vitest';
import { computeLocalDiff } from '../../src/lib/diff/compute';

describe('computeLocalDiff', () => {
  it('identical texts', () => {
    const r = computeLocalDiff('hello world', 'hello world');
    expect(r.changes.every((c) => c.op === 'Equal')).toBe(true);
    expect(r.stats.similarity).toBe(100);
  });

  it('insertion', () => {
    const r = computeLocalDiff('hello world', 'hello beautiful world');
    expect(r.changes.some((c) => c.op === 'Insert')).toBe(true);
    expect(r.changes.some((c) => c.op === 'Equal')).toBe(true);
  });

  it('deletion', () => {
    const r = computeLocalDiff('hello beautiful world', 'hello world');
    expect(r.changes.some((c) => c.op === 'Delete')).toBe(true);
  });

  it('empty inputs returns 100% similarity', () => {
    const r = computeLocalDiff('', '');
    expect(r.changes).toHaveLength(0);
    expect(r.stats.similarity).toBe(100);
  });

  it('similarity score with replacements', () => {
    const r = computeLocalDiff('hello world foo bar', 'hello world baz qux');
    expect(r.stats.similarity).toBeCloseTo(50, 0);
  });

  it('completely different texts have 0% similarity', () => {
    const r = computeLocalDiff('hello world', 'goodbye universe');
    expect(r.stats.similarity).toBe(0);
    expect(r.changes.every((c) => c.op === 'Replace')).toBe(true);
  });

  it('kurdish text', () => {
    const r = computeLocalDiff('ئەم کەسە لە ساڵەکانی ١٩٥٠دا دەژیا', 'ئەم کەسە لە ساڵانی 1950دا دەژیا');
    expect(r.changes.some((c) => c.op === 'Replace')).toBe(true);
  });

  it('partial overlap', () => {
    const r = computeLocalDiff('a b c d', 'a x c y');
    expect(r.stats.unchanged_words).toBe(2);
    expect(r.stats.changed_words).toBe(2);
  });

  it('mismatched lengths', () => {
    const r = computeLocalDiff('a b', 'a b c d e');
    expect(r.stats.unchanged_words).toBe(2);
    expect(r.stats.added_words).toBe(3);
  });

  it('guards oversized inputs without allocating an LCS table', () => {
    const huge = Array.from({ length: 10_001 }, (_, idx) => `w${idx}`).join(' ');
    const r = computeLocalDiff(huge, 'short text');
    expect(r.changes).toHaveLength(0);
    expect(r.stats.similarity).toBe(100);
  });
});
