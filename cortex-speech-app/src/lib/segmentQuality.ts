import type { SpeechSegment } from './types';

/**
 * True when a human explicitly REJECTED this clip — the review "mark bad" action, or a jury/import
 * `human_reject` verdict. The clip is KEPT in the library (reversible: re-review to accept, or
 * re-transcribe) but must be excluded from every "verified"/good-quality count and from any export of
 * verified data. Mirrors the Rust `quality::is_human_rejected` so the UI counts match the exported
 * dataset exactly.
 *
 * `s.verified` alone is not a sufficient "confirmed-good" test: `markBad` sets `verified = true` to
 * finalize a rejected clip out of the review queue, so a plain `s.verified` filter would miscount a
 * rejected clip as confirmed-good output.
 */
export function isHumanRejected(seg: Pick<SpeechSegment, 'humanDecision' | 'verdict'>): boolean {
  const hd = (seg.humanDecision ?? '').toLowerCase();
  const vd = (seg.verdict ?? '').toLowerCase();
  return hd === 'reject' || hd === 'human_reject' || vd === 'human_reject';
}

/** A clip a human has confirmed GOOD: verified (accept/edit) and NOT rejected. */
export function isVerifiedGood(seg: Pick<SpeechSegment, 'verified' | 'humanDecision' | 'verdict'>): boolean {
  return seg.verified && !isHumanRejected(seg);
}

/**
 * True when a transcript is an ASR PLACEHOLDER, not real text — the sentinel a not-yet-transcribed or
 * failed 7B segment carries ("[Pending WSL 7B ASR]", "[ASR unavailable...]"). Mirrors the Rust
 * `quality::is_placeholder_transcript` so the UI never lets the owner "verify" a placeholder as gold
 * (the backend already excludes it from the training grade — this keeps the review counts honest too).
 */
export function isPlaceholderTranscript(text: string | null | undefined): boolean {
  const t = (text ?? '').trim();
  const lower = t.toLowerCase();
  return t.startsWith('[ASR unavailable') || t.startsWith('[Pending') || lower === 'n/a' || lower === 'null';
}

/**
 * True when a segment carries at least one REAL transcript — a non-empty, non-placeholder string in any of
 * its transcript fields — i.e. actual content that could ship, not just an ASR placeholder or blanks. Used
 * to keep "verified" COUNTS honest: batch_verify sets verified=true with NO content guard, so "Verify all
 * pending" marks a still-pending placeholder clip (awaiting the 7B) as verified — yet export_dataset drops
 * every placeholder/empty row via the training grade, so a plain `verified` tally over-counts what the
 * dataset can actually contain. Correct-direction by design: a clip with ANY real transcript is real, so this
 * never under-counts a genuinely-good clip; it excludes only clips that are placeholder/empty EVERYWHERE.
 */
export function hasRealTranscript(
  seg: Pick<SpeechSegment, 'rawTranscript' | 'normalizedTranscript' | 'annotatedTranscript' | 'verdictTranscript'>,
): boolean {
  return [seg.annotatedTranscript, seg.verdictTranscript, seg.normalizedTranscript, seg.rawTranscript].some((t) => {
    const s = (t ?? '').trim();
    return s.length > 0 && !isPlaceholderTranscript(s);
  });
}
