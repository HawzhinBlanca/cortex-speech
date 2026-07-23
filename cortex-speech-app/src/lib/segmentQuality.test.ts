import { describe, it, expect } from 'vitest';
import { hasRealTranscript, isVerifiedGood, isHumanRejected } from './segmentQuality';
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
    expect(hasRealTranscript(seg({ rawTranscript: '[Pending WSL 7B ASR]', annotatedTranscript: 'کوردی' }))).toBe(true);
    expect(hasRealTranscript(seg({ rawTranscript: '', normalizedTranscript: 'سڵاو' }))).toBe(true);
  });

  it('a batch-verified placeholder is NOT a good verified clip (verified but no real content)', () => {
    const ph = seg({ verified: true, rawTranscript: '[Pending WSL 7B ASR]' });
    expect(isVerifiedGood(ph)).toBe(true); // verified + not rejected
    expect(isHumanRejected(ph)).toBe(false);
    expect(hasRealTranscript(ph)).toBe(false); // ...but no shippable content -> excluded from the verified count
  });
});
