import { describe, expect, it } from 'vitest';
import {
  addAbsolutePlaybackInterval,
  addPlaybackInterval,
  emptyPlaybackCoverage,
} from './playbackCoverage';

describe('unique playback coverage', () => {
  it('counts the same half only once when it is replayed', () => {
    let coverage = emptyPlaybackCoverage();
    coverage = addPlaybackInterval(coverage, 0, 5_000, 10_000);
    const afterFirstListen = coverage;
    coverage = addPlaybackInterval(coverage, 0, 5_000, 10_000);

    expect(coverage.uniqueMs).toBe(5_000);
    expect(coverage.intervals).toEqual([{ startMs: 0, endMs: 5_000 }]);
    expect(coverage, 'a wholly duplicated replay is a reducer no-op').toBe(afterFirstListen);
  });

  it('unions overlap and adjacency without double-counting', () => {
    let coverage = emptyPlaybackCoverage();
    coverage = addPlaybackInterval(coverage, 0, 6_000, 10_000);
    coverage = addPlaybackInterval(coverage, 4_000, 8_000, 10_000);
    coverage = addPlaybackInterval(coverage, 8_000, 10_000, 10_000);

    expect(coverage).toEqual({
      intervals: [{ startMs: 0, endMs: 10_000 }],
      uniqueMs: 10_000,
    });
  });

  it('preserves disjoint ranges and sums only their union', () => {
    let coverage = emptyPlaybackCoverage();
    coverage = addPlaybackInterval(coverage, 8_000, 10_000, 10_000);
    coverage = addPlaybackInterval(coverage, 0, 2_000, 10_000);

    expect(coverage).toEqual({
      intervals: [
        { startMs: 0, endMs: 2_000 },
        { startMs: 8_000, endMs: 10_000 },
      ],
      uniqueMs: 4_000,
    });
  });

  it('anchors a nonzero source offset to the full database segment, not a spoken subset', () => {
    let coverage = emptyPlaybackCoverage();
    // DB segment is source [100s,110s]. A tapped word at [102s,103s] is clip [2s,3s], not [0s,1s].
    coverage = addAbsolutePlaybackInterval(coverage, 102, 103, 100, 10);
    expect(coverage).toEqual({
      intervals: [{ startMs: 2_000, endMs: 3_000 }],
      uniqueMs: 1_000,
    });

    // Completing the full segment can honestly cross the 85% backend threshold even when word
    // timings cover only the middle; replay/word playback and decision evidence share coordinates.
    coverage = addAbsolutePlaybackInterval(coverage, 100, 108.5, 100, 10);
    expect(coverage.uniqueMs).toBe(8_500);
    expect(coverage.intervals).toEqual([{ startMs: 0, endMs: 8_500 }]);
  });

  it('fails conservatively for invalid values and clips observations to the review window', () => {
    const empty = emptyPlaybackCoverage();
    expect(addPlaybackInterval(empty, 0, Number.NaN, 10_000)).toBe(empty);
    expect(addPlaybackInterval(empty, 0, 1_000, Number.POSITIVE_INFINITY)).toBe(empty);
    expect(addPlaybackInterval(empty, 2_000, 1_000, 10_000)).toBe(empty);

    const bounded = addPlaybackInterval(empty, -100.8, 10_100.9, 10_000.9);
    expect(bounded).toEqual({
      intervals: [{ startMs: 0, endMs: 10_000 }],
      uniqueMs: 10_000,
    });
  });

  it('matches an independent millisecond bitmap through 10,000 randomized observations', () => {
    const clipDurationMs = 4_096;
    const heard = new Uint8Array(clipDurationMs);
    let coverage = emptyPlaybackCoverage();
    let seed = 0x5eed1234;
    let previous = { startMs: 0, endMs: 0 };

    const random = () => {
      seed = (Math.imul(seed, 1_664_525) + 1_013_904_223) >>> 0;
      return seed;
    };

    for (let iteration = 0; iteration < 10_000; iteration += 1) {
      let startMs = (random() % (clipDurationMs + 1_024)) - 512;
      let endMs = startMs + (random() % 900);
      if (iteration % 17 === 0) ({ startMs, endMs } = previous); // deliberate replay
      if (iteration % 43 === 0) [startMs, endMs] = [endMs, startMs]; // invalid reversal
      previous = { startMs, endMs };

      coverage = addPlaybackInterval(coverage, startMs, endMs, clipDurationMs);
      if (endMs > startMs) {
        const boundedStart = Math.min(clipDurationMs, Math.max(0, startMs));
        const boundedEnd = Math.min(clipDurationMs, Math.max(0, endMs));
        for (let ms = boundedStart; ms < boundedEnd; ms += 1) heard[ms] = 1;
      }

      if (iteration % 127 === 0) {
        expect(coverage.uniqueMs).toBe(heard.reduce((total, value) => total + value, 0));
        for (let index = 1; index < coverage.intervals.length; index += 1) {
          expect(coverage.intervals[index - 1].endMs).toBeLessThan(
            coverage.intervals[index].startMs,
          );
        }
      }
    }

    expect(coverage.uniqueMs).toBe(heard.reduce((total, value) => total + value, 0));
  });
});
