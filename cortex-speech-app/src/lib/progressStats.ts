// Pure progress math for the processing indicator — percent + ETA from (current, total, elapsed).
// Kept separate from the Svelte component so it is unit-tested in isolation (no DOM, no stores).

export interface ProgressStats {
  /** True when a real percentage can be shown (total known and > 0); else the bar is indeterminate. */
  hasPercent: boolean;
  /** 0–100, clamped and rounded. 0 when the total is unknown. */
  percent: number;
  /** Estimated milliseconds remaining, or null when not yet computable (no items done, or no total). */
  etaMs: number | null;
  /** Items completed, clamped to [0, total] when a total is known. */
  done: number;
  /** Total items, or 0 when unknown. */
  total: number;
}

/**
 * Compute display stats for a determinate-or-indeterminate progress bar.
 *
 * ETA uses a simple linear extrapolation (elapsed-per-done × remaining) — honest and stable for the
 * roughly-uniform per-chunk cost of ASR. It is null until at least one item is done AND time has
 * passed (so we never show a wildly wrong estimate at t=0), and 0 once done ≥ total.
 */
export function computeProgress(current: number, total: number, elapsedMs: number): ProgressStats {
  const t = Number.isFinite(total) && total > 0 ? Math.floor(total) : 0;
  const rawDone = Number.isFinite(current) && current > 0 ? Math.floor(current) : 0;
  const elapsed = Number.isFinite(elapsedMs) && elapsedMs > 0 ? elapsedMs : 0;

  if (t === 0) {
    return { hasPercent: false, percent: 0, etaMs: null, done: rawDone, total: 0 };
  }

  const done = Math.min(rawDone, t);
  const percent = Math.min(100, Math.max(0, Math.round((done / t) * 100)));

  let etaMs: number | null = null;
  if (done >= t) {
    etaMs = 0;
  } else if (done > 0 && elapsed > 0) {
    etaMs = Math.round((elapsed / done) * (t - done));
  }

  return { hasPercent: true, percent, etaMs, done, total: t };
}

/** Compact human duration: "45s", "2m 05s", "1h 03m". Negative/NaN → "0s". */
export function formatDurationShort(ms: number): string {
  const totalSec = Number.isFinite(ms) && ms > 0 ? Math.round(ms / 1000) : 0;
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = totalSec % 60;
  if (h > 0) return `${h}h ${String(m).padStart(2, '0')}m`;
  if (m > 0) return `${m}m ${String(s).padStart(2, '0')}s`;
  return `${s}s`;
}
