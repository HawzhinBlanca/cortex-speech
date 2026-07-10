import type { WordTimestamp } from './types';

/**
 * Playback window for hearing EXACTLY one word (absolute file time).
 *
 * Forced-alignment boundaries routinely clip word onsets/codas, so pad both edges slightly,
 * then clamp to the clip window ([clipStart, clipEnd]; clipEnd <= clipStart means "unbounded",
 * matching chunkPlaybackRange's null-meta contract). The window is floored to a small minimum
 * so a degenerate word timing (start == end) still produces an audible, stoppable play.
 */
export function wordPlayBounds(
  w: Pick<WordTimestamp, 'start' | 'end'>,
  clipStart: number,
  clipEnd: number,
  pad = 0.12,
): { start: number; end: number } {
  const MIN_WINDOW = 0.05;
  const start = Math.max(clipStart, clipStart + w.start - pad);
  let end = clipStart + w.end + pad;
  if (clipEnd > clipStart) end = Math.min(clipEnd, end);
  return { start, end: Math.max(end, start + MIN_WINDOW) };
}

/**
 * Replace one whitespace-delimited token of `text` with `replacement` and return the new
 * string — or null when the token cannot be located CONFIDENTLY, so callers fall back to a
 * manual selection instead of guess-rewriting a transcript (the gold-label surface must never
 * corrupt).
 *
 * "Confidently" is strict, because Sorani function words (لە، و، بە) repeat constantly and a
 * wrong-occurrence rewrite is silent gold corruption:
 *   1. Positional pick — trust token `index` ONLY when the token stream still lines up with the
 *      word list (`expectedTokenCount` matches) AND that token equals `expected`. A count
 *      mismatch means a prior edit drifted the indices, so the position is no longer reliable.
 *   2. Otherwise fall back to an exact match ONLY when it is UNIQUE. A repeated word is
 *      ambiguous — return null rather than pick the first occurrence.
 */
export function replaceWordToken(
  text: string,
  index: number,
  expected: string,
  replacement: string,
  expectedTokenCount?: number,
): string | null {
  const tokens: Array<{ start: number; len: number; word: string }> = [];
  const re = /\S+/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) tokens.push({ start: m.index, len: m[0].length, word: m[0] });

  const splice = (t: { start: number; len: number }) =>
    text.slice(0, t.start) + replacement + text.slice(t.start + t.len);

  const alignmentIntact = expectedTokenCount === undefined || tokens.length === expectedTokenCount;
  if (alignmentIntact && tokens[index]?.word === expected) {
    return splice(tokens[index]);
  }
  const matches = tokens.filter((t) => t.word === expected);
  if (matches.length === 1) return splice(matches[0]);
  return null;
}
