export interface PlaybackInterval {
  /** Inclusive clip-relative start, in whole milliseconds. */
  startMs: number;
  /** Exclusive clip-relative end, in whole milliseconds. */
  endMs: number;
}

export interface PlaybackCoverage {
  /** Sorted, non-overlapping and non-adjacent clip-relative intervals. */
  intervals: readonly PlaybackInterval[];
  /** Exact length of the interval union. Replayed/overlapping media is counted once. */
  uniqueMs: number;
}

export function emptyPlaybackCoverage(): PlaybackCoverage {
  return { intervals: [], uniqueMs: 0 };
}

/**
 * Add one observed, continuously-played media interval to a canonical clip-relative union.
 *
 * The caller owns discontinuity detection (seek/loop/source/attempt boundaries). This reducer owns
 * the facts that are easiest to accidentally get subtly wrong: finite/bounded input, conservative
 * whole-millisecond rounding, ordering, overlap removal and exact union length.
 */
export function addPlaybackInterval(
  coverage: PlaybackCoverage,
  startMs: number,
  endMs: number,
  clipDurationMs: number,
): PlaybackCoverage {
  if (
    !Number.isFinite(startMs) ||
    !Number.isFinite(endMs) ||
    !Number.isFinite(clipDurationMs) ||
    clipDurationMs <= 0 ||
    endMs <= startMs
  ) {
    return coverage;
  }

  const limit = Math.max(0, Math.floor(clipDurationMs));
  // Round inward: fractional observation boundaries must never manufacture a millisecond that was
  // not actually traversed. The backend accepts an integer counter, so conservative loss is safer
  // than fractional inflation at every timeupdate boundary.
  const start = Math.min(limit, Math.max(0, Math.ceil(startMs)));
  const end = Math.min(limit, Math.max(0, Math.floor(endMs)));
  if (end <= start) return coverage;

  const merged: PlaybackInterval[] = [];
  let nextStart = start;
  let nextEnd = end;
  let inserted = false;

  for (const interval of coverage.intervals) {
    if (interval.endMs < nextStart) {
      merged.push(interval);
      continue;
    }
    if (nextEnd < interval.startMs) {
      if (!inserted) {
        merged.push({ startMs: nextStart, endMs: nextEnd });
        inserted = true;
      }
      merged.push(interval);
      continue;
    }

    // Overlap OR adjacency: both are one canonical interval and contribute no duplicated duration.
    nextStart = Math.min(nextStart, interval.startMs);
    nextEnd = Math.max(nextEnd, interval.endMs);
  }

  if (!inserted) merged.push({ startMs: nextStart, endMs: nextEnd });
  const uniqueMs = merged.reduce((total, interval) => total + interval.endMs - interval.startMs, 0);

  // Preserve referential identity when a replay was wholly covered already. Besides avoiding a
  // redundant Svelte update, this makes "replay adds nothing" explicit in the reducer contract.
  if (
    uniqueMs === coverage.uniqueMs &&
    merged.length === coverage.intervals.length &&
    merged.every(
      (interval, index) =>
        interval.startMs === coverage.intervals[index]?.startMs &&
        interval.endMs === coverage.intervals[index]?.endMs,
    )
  ) {
    return coverage;
  }

  return { intervals: merged, uniqueMs };
}

/** Convert one absolute source-media progression into the database segment's coordinate system. */
export function addAbsolutePlaybackInterval(
  coverage: PlaybackCoverage,
  absoluteStartSeconds: number,
  absoluteEndSeconds: number,
  evidenceStartSeconds: number,
  evidenceDurationSeconds: number,
): PlaybackCoverage {
  if (
    !Number.isFinite(absoluteStartSeconds) ||
    !Number.isFinite(absoluteEndSeconds) ||
    !Number.isFinite(evidenceStartSeconds) ||
    !Number.isFinite(evidenceDurationSeconds)
  ) {
    return coverage;
  }
  return addPlaybackInterval(
    coverage,
    (absoluteStartSeconds - evidenceStartSeconds) * 1000,
    (absoluteEndSeconds - evidenceStartSeconds) * 1000,
    evidenceDurationSeconds * 1000,
  );
}
