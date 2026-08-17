import { describe, it, expect } from 'vitest';
import {
  hasRealTranscript,
  isVerifiedGood,
  isHumanRejected,
  effectiveTranscript,
} from './segmentQuality';
import type { SpeechSegment } from './types';

const seg = (over: Partial<SpeechSegment>): SpeechSegment =>
  ({
    id: 's',
    audioPath: '/a.wav',
    rawTranscript: '',
    normalizedTranscript: null,
    annotatedTranscript: null,
    durationMs: 1000,
    verified: false,
    ...over,
  }) as unknown as SpeechSegment;

describe('hasRealTranscript', () => {
  it('is false when every transcript field is empty or a placeholder', () => {
    // The reachable case: a still-pending placeholder clip caught by "Verify all pending".
    expect(hasRealTranscript(seg({ rawTranscript: '[Pending WSL 7B ASR]' }))).toBe(false);
    expect(hasRealTranscript(seg({ rawTranscript: '[ASR unavailable: engine down]' }))).toBe(false);
    expect(hasRealTranscript(seg({ rawTranscript: '   ' }))).toBe(false);
    expect(hasRealTranscript(seg({ rawTranscript: 'n/a' }))).toBe(false);
  });

  it('is true when ANY field carries real content (never under-counts a good clip)', () => {
    expect(hasRealTranscript(seg({ rawTranscript: 'کوردی' }))).toBe(true);
    // A placeholder raw but a real human annotation is still real content.
    expect(
      hasRealTranscript(
        seg({ rawTranscript: '[Pending WSL 7B ASR]', annotatedTranscript: 'کوردی' }),
      ),
    ).toBe(true);
    expect(hasRealTranscript(seg({ rawTranscript: '', normalizedTranscript: 'سڵاو' }))).toBe(true);
  });

  it('a batch-verified placeholder is NOT a good verified clip (verified but no real content)', () => {
    const ph = seg({ verified: true, rawTranscript: '[Pending WSL 7B ASR]' });
    expect(isVerifiedGood(ph)).toBe(true); // verified + not rejected
    expect(isHumanRejected(ph)).toBe(false);
    expect(hasRealTranscript(ph)).toBe(false); // ...but no shippable content -> excluded from the verified count
  });
});

describe('effectiveTranscript', () => {
  const base = {
    rawTranscript: 'raw',
    annotatedTranscript: null,
    verdictTranscript: null,
    humanDecision: null,
    verdict: null,
  };

  it('prefers the verdict transcript when a human decided', () => {
    // The rule the old stats order missed entirely: a human edit lives in verdictTranscript, and
    // measuring annotated/raw instead reports the text the human REPLACED.
    for (const humanDecision of ['accept', 'edit', 'human_accept', 'human_edit', 'EDIT']) {
      expect(effectiveTranscript({ ...base, verdictTranscript: 'corrected', humanDecision })).toBe(
        'corrected',
      );
    }
    expect(
      effectiveTranscript({ ...base, verdictTranscript: 'corrected', verdict: 'human_accept' }),
    ).toBe('corrected');
  });

  it('ignores a verdict transcript that no human decision stands behind', () => {
    // A machine verdict is not a human edit — annotated still wins.
    expect(
      effectiveTranscript({
        ...base,
        annotatedTranscript: 'annotated',
        verdictTranscript: 'machine',
      }),
    ).toBe('annotated');
  });

  it('falls back through annotated then verbatim raw — machine text never surfaces (VERBATIM LAW)', () => {
    expect(effectiveTranscript({ ...base, annotatedTranscript: 'annotated' })).toBe('annotated');
    // An undecided machine verdict is evidence, not the transcript: the champion's verbatim raw wins.
    expect(effectiveTranscript({ ...base, verdictTranscript: 'machine' })).toBe('raw');
    expect(effectiveTranscript(base)).toBe('raw');
  });

  it('treats whitespace-only fields as absent rather than selecting them', () => {
    expect(effectiveTranscript({ ...base, annotatedTranscript: '   ' })).toBe('raw');
  });

  it('returns an empty string when nothing is present', () => {
    // rawTranscript is `string`, not `string | null` — an absent raw transcript is '' by contract.
    expect(effectiveTranscript({ ...base, rawTranscript: '' })).toBe('');
  });
});
