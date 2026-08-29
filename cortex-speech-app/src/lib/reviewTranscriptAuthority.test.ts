import { describe, expect, it } from 'vitest';
import { reviewTranscript } from './reviewTranscriptAuthority';
import type { SpeechSegment } from './types';

function segment(overrides: Partial<SpeechSegment> = {}): SpeechSegment {
  return {
    id: 'authority-test',
    audioPath: 'C:\\audio\\authority.wav',
    rawTranscript: 'champion raw',
    normalizedTranscript: 'machine refinement',
    annotatedTranscript: null,
    verdictTranscript: null,
    alignmentJson: null,
    durationMs: 1_000,
    speakerId: null,
    verified: false,
    ...overrides,
  };
}

describe('review transcript authority', () => {
  it('uses a frozen verdict only behind a human text-decision marker', () => {
    expect(
      reviewTranscript(
        segment({ humanDecision: 'edit', verdictTranscript: 'frozen human correction' }),
      ),
    ).toBe('frozen human correction');
    expect(
      reviewTranscript(
        segment({ verdict: 'human_accept', verdictTranscript: 'frozen human acceptance' }),
      ),
    ).toBe('frozen human acceptance');
  });

  it('uses a nonblank human annotation before champion raw', () => {
    expect(reviewTranscript(segment({ annotatedTranscript: 'human annotation' }))).toBe(
      'human annotation',
    );
    expect(reviewTranscript(segment({ annotatedTranscript: '   ' }))).toBe('champion raw');
  });

  it('never promotes a machine verdict or normalized refinement into review truth', () => {
    expect(
      reviewTranscript(
        segment({ verdict: 'jury_accept', verdictTranscript: 'machine jury proposal' }),
      ),
    ).toBe('champion raw');
  });
});
