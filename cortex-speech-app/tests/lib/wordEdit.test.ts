import { describe, it, expect } from 'vitest';
import { wordPlayBounds, replaceWordToken } from '../../src/lib/wordEdit';

describe('wordPlayBounds', () => {
  it('pads both edges and offsets by the clip start', () => {
    const b = wordPlayBounds({ start: 1.0, end: 1.5 }, 10, 20, 0.12);
    expect(b.start).toBeCloseTo(10.88);
    expect(b.end).toBeCloseTo(11.62);
  });

  it('clamps to the clip window (first/last word of a chunk)', () => {
    const b = wordPlayBounds({ start: 0.05, end: 9.95 }, 10, 20, 0.12);
    expect(b.start).toBe(10); // 10.05 - 0.12 would under-run the clip
    expect(b.end).toBe(20); // 19.95 + 0.12 would bleed into the next chunk
  });

  it('does not clamp the end for an unbounded clip (clipEnd <= clipStart means whole file)', () => {
    const b = wordPlayBounds({ start: 3.0, end: 3.4 }, 0, 0, 0.12);
    expect(b.start).toBeCloseTo(2.88);
    expect(b.end).toBeCloseTo(3.52);
  });

  it('floors a degenerate word timing to an audible, stoppable window', () => {
    const b = wordPlayBounds({ start: 2.0, end: 2.0 }, 0, 0, 0);
    expect(b.end - b.start).toBeGreaterThan(0);
  });

  it('never re-expands a degenerate last word beyond the clip end', () => {
    const b = wordPlayBounds({ start: 1, end: 1 }, 10, 11, 0);
    expect(b.end).toBe(11);
    expect(b.start).toBeCloseTo(10.95);
  });

  it('never starts before the clip even with a large pad', () => {
    const b = wordPlayBounds({ start: 0, end: 0.3 }, 5, 6, 1.0);
    expect(b.start).toBe(5);
    expect(b.end).toBe(6);
  });
});

describe('replaceWordToken', () => {
  // Real Sorani (RTL, Arabic-block) — the surface this feature exists for.
  const sorani = 'خوات لەگەڵ بێت ئەمڕۆ';

  it('replaces the index-th token when the alignment is intact', () => {
    expect(replaceWordToken(sorani, 1, 'لەگەڵ', 'لەگەڵمان', 4)).toBe('خوات لەگەڵمان بێت ئەمڕۆ');
  });

  it('picks the CORRECT repeated occurrence by position when the token count corroborates it', () => {
    const text = 'کە وشە کە دیسان'; // "کە" at index 0 and 2
    // index 2 is the SECOND "کە"; count matches (4) so the position is trusted — the first stays put.
    expect(replaceWordToken(text, 2, 'کە', 'X', 4)).toBe('کە وشە X دیسان');
  });

  it('refuses to guess a repeated word once indices have drifted (returns null, not the first match)', () => {
    // A prior multi-word edit grew editText to 5 tokens while the chip list still has 4 → count
    // mismatch → the position is untrusted, and "لە" repeats (index 0 and 3), so it must NOT
    // rewrite blindly.
    const drifted = 'لە مالی گەورە لە شار'; // 5 tokens, "لە" twice
    expect(replaceWordToken(drifted, 2, 'لە', 'BA', 4)).toBeNull();
  });

  it('rejects a positional false-positive when the count mismatches (different occurrence at index)', () => {
    // chips=[B,B,C] (3) but editText inserted an X → "X B B C" (4). index 1 is the FIRST B in text,
    // but chip 1 is the SECOND B — trusting position would corrupt. Ambiguous "B" ⇒ null.
    expect(replaceWordToken('X B B C', 1, 'B', 'Z', 3)).toBeNull();
  });

  it('falls back to a UNIQUE exact match when the index has drifted', () => {
    // The reviewer inserted a word, shifting indices; "لەگەڵ" is still unique so it is safe.
    expect(replaceWordToken('نوێ خوات لەگەڵ بێت', 1, 'لەگەڵ', 'Y', 3)).toBe('نوێ خوات Y بێت');
  });

  it('returns null when the word cannot be located (never guess-rewrite)', () => {
    expect(replaceWordToken(sorani, 0, 'نییە', 'Z', 4)).toBeNull();
    expect(replaceWordToken('', 0, 'خوات', 'Z')).toBeNull();
  });

  it('returns null when the token text has punctuation the alignment word lacks', () => {
    // "بێت،" !== "بێت" — rewriting here would silently eat the reviewer's punctuation.
    expect(replaceWordToken('خوات لەگەڵ بێت،', 2, 'بێت', 'Z')).toBeNull();
  });

  it('preserves surrounding whitespace exactly', () => {
    expect(replaceWordToken('a  b\tc', 1, 'b', 'BB')).toBe('a  BB\tc');
  });

  it('supports a multi-word replacement (one heard token → two typed words)', () => {
    expect(replaceWordToken('a b c', 1, 'b', 'x y')).toBe('a x y c');
  });
});
