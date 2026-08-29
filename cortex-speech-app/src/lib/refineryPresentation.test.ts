import { describe, expect, it } from 'vitest';
import {
  formatRefineryMetric,
  formatRefineryPercent,
  isolateRefineryValue,
} from './refineryPresentation';

describe('refinery presentation', () => {
  it('formats measured rates without presenting empty evidence as a perfect score', () => {
    expect(formatRefineryPercent(0.125)).toBe('12.5%');
    expect(formatRefineryMetric(0, 0)).toBe('—');
    expect(formatRefineryMetric(0.125, 8)).toBe('12.5%');
  });

  it('isolates interpolated values from surrounding bidirectional text', () => {
    expect(isolateRefineryValue('12.5%')).toBe('\u206812.5%\u2069');
    expect(isolateRefineryValue(42)).toBe('\u206842\u2069');
  });
});
