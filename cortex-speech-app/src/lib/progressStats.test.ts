import { describe, it, expect } from 'vitest';
import { computeProgress, formatDurationShort } from './progressStats';

describe('computeProgress', () => {
  it('is indeterminate when total is unknown', () => {
    const p = computeProgress(0, 0, 5000);
    expect(p.hasPercent).toBe(false);
    expect(p.percent).toBe(0);
    expect(p.etaMs).toBeNull();
  });

  it('computes percent and clamps done to total', () => {
    expect(computeProgress(50, 100, 0).percent).toBe(50);
    expect(computeProgress(144, 144, 0).percent).toBe(100);
    // a stray over-count never exceeds 100%
    expect(computeProgress(200, 144, 0).percent).toBe(100);
  });

  it('does not show an ETA before any item is done (avoids a wild t=0 estimate)', () => {
    expect(computeProgress(0, 100, 3000).etaMs).toBeNull();
  });

  it('extrapolates ETA linearly from elapsed-per-done', () => {
    // 25 of 100 done in 10s -> 0.4s/item -> 75 remaining -> 30s
    expect(computeProgress(25, 100, 10_000).etaMs).toBe(30_000);
  });

  it('reports ETA 0 once complete', () => {
    expect(computeProgress(100, 100, 12_345).etaMs).toBe(0);
  });

  it('is robust to NaN / negative inputs', () => {
    const p = computeProgress(NaN, -5, NaN);
    expect(p).toEqual({ hasPercent: false, percent: 0, etaMs: null, done: 0, total: 0 });
  });
});

describe('formatDurationShort', () => {
  it('formats seconds, minutes, hours', () => {
    expect(formatDurationShort(45_000)).toBe('45s');
    expect(formatDurationShort(125_000)).toBe('2m 05s');
    expect(formatDurationShort(3_780_000)).toBe('1h 03m');
  });
  it('floors negatives and NaN to 0s', () => {
    expect(formatDurationShort(-1)).toBe('0s');
    expect(formatDurationShort(NaN)).toBe('0s');
  });
});
