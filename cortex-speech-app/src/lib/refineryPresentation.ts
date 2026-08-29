export const formatRefineryPercent = (value: number): string => `${(value * 100).toFixed(1)}%`;

export const isolateRefineryValue = (value: string | number): string =>
  `${String.fromCodePoint(0x2068)}${String(value)}${String.fromCodePoint(0x2069)}`;

// A rate over no scored segments is undefined, never a perfect 0%.
export const formatRefineryMetric = (value: number, segmentCount: number): string =>
  segmentCount > 0 ? formatRefineryPercent(value) : '—';
