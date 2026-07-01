import { describe, it, expect } from 'vitest';
import { isHumanRejected, isVerifiedGood } from '../../src/lib/segmentQuality';

describe('segmentQuality', () => {
  it('flags a review "mark bad" clip (verdict=human_reject) as rejected', () => {
    expect(isHumanRejected({ humanDecision: 'reject', verdict: 'human_reject' })).toBe(true);
    // verdict alone, or humanDecision alone, is enough
    expect(isHumanRejected({ humanDecision: null, verdict: 'human_reject' })).toBe(true);
    expect(isHumanRejected({ humanDecision: 'reject', verdict: null })).toBe(true);
  });

  it('does not flag accepted / edited / pending clips as rejected', () => {
    expect(isHumanRejected({ humanDecision: 'accept', verdict: 'human_accept' })).toBe(false);
    expect(isHumanRejected({ humanDecision: 'edit', verdict: 'human_edit' })).toBe(false);
    expect(isHumanRejected({ humanDecision: null, verdict: null })).toBe(false);
    expect(isHumanRejected({ humanDecision: undefined, verdict: undefined })).toBe(false);
  });

  it('isVerifiedGood is true only for verified AND not-rejected clips', () => {
    // A human-rejected clip carries verified=true only to leave the review queue — NOT confirmed-good.
    expect(isVerifiedGood({ verified: true, humanDecision: 'reject', verdict: 'human_reject' })).toBe(false);
    expect(isVerifiedGood({ verified: true, humanDecision: 'accept', verdict: 'human_accept' })).toBe(true);
    expect(isVerifiedGood({ verified: true, humanDecision: null, verdict: null })).toBe(true);
    expect(isVerifiedGood({ verified: false, humanDecision: null, verdict: null })).toBe(false);
  });
});
