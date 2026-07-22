import { describe, it, expect } from 'vitest';
import { computeLocalDiff } from './compute';

describe('computeLocalDiff', () => {
  it('marks identical text as fully unchanged', () => {
    const d = computeLocalDiff('hello world', 'hello world');
    expect(d.changes.every((c) => c.op === 'Equal')).toBe(true);
    expect(d.stats.similarity).toBe(100);
  });

  it('an insertion next to a common word is an Insert, not a Replace (regression)', () => {
    // "a c" -> "a b c" is a pure insertion of b. The old reconstruction consumed the common word c
    // into a bogus Replace(c->b) and then Inserted c, undercounting similarity (33% instead of 67%).
    const d = computeLocalDiff('a c', 'a b c');
    expect(d.stats.unchanged_words).toBe(2);
    expect(d.stats.added_words).toBe(1);
    expect(d.stats.changed_words).toBe(0);
    expect(d.stats.removed_words).toBe(0);
    expect(d.stats.similarity).toBeCloseTo(200 / 3, 5);
    expect(d.changes.map((c) => c.op)).toEqual(['Equal', 'Insert', 'Equal']);
  });

  it('a deletion next to a common word is a Delete, not a Replace (regression)', () => {
    const d = computeLocalDiff('a b c', 'a c');
    expect(d.stats.unchanged_words).toBe(2);
    expect(d.stats.removed_words).toBe(1);
    expect(d.stats.changed_words).toBe(0);
    expect(d.stats.added_words).toBe(0);
    expect(d.changes.map((c) => c.op)).toEqual(['Equal', 'Delete', 'Equal']);
  });

  it('a genuine substitution is a Replace', () => {
    const d = computeLocalDiff('a b c', 'a x c');
    expect(d.stats.unchanged_words).toBe(2);
    expect(d.stats.changed_words).toBe(1);
    expect(d.stats.similarity).toBeCloseTo(200 / 3, 5);
  });

  it('every operation accounts for all raw and annotated words', () => {
    const pairs: Array<[string, string]> = [
      ['a b c d', 'a x c y'],
      ['', 'a b'],
      ['a b', ''],
      ['a c', 'a b c'],
      ['the quick brown fox', 'the slow brown cat jumps'],
    ];
    for (const [raw, ann] of pairs) {
      const d = computeLocalDiff(raw, ann);
      const rawWords = raw.split(/\s+/).filter(Boolean);
      const annWords = ann.split(/\s+/).filter(Boolean);
      const eq = d.changes.filter((c) => c.op === 'Equal').length;
      const ins = d.changes.filter((c) => c.op === 'Insert').length;
      const del = d.changes.filter((c) => c.op === 'Delete').length;
      const rep = d.changes.filter((c) => c.op === 'Replace').length;
      expect(eq + del + rep).toBe(rawWords.length);
      expect(eq + ins + rep).toBe(annWords.length);
      expect(d.stats.similarity).toBeGreaterThanOrEqual(0);
      expect(d.stats.similarity).toBeLessThanOrEqual(100);
    }
  });
});
