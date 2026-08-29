import {
  addAbsolutePlaybackInterval,
  emptyPlaybackCoverage,
  type PlaybackCoverage,
  type PlaybackInterval,
} from './playbackCoverage';

interface PlaybackEvidencePort {
  heardMs: () => number;
  heardIntervals: () => readonly PlaybackInterval[];
  evidenceMode: () => boolean;
  evidenceOrigin: () => number;
  evidenceLength: () => number;
  setHeardMs: (value: number) => void;
  setHeardIntervals: (value: readonly PlaybackInterval[]) => void;
}

export class AudioPlaybackEvidence {
  private lastMediaPosition: number | null = null;
  private coverage: PlaybackCoverage = emptyPlaybackCoverage();

  constructor(private readonly port: PlaybackEvidencePort) {}

  private publish(value: number): void {
    if (this.port.heardMs() !== value) this.port.setHeardMs(value);
    const previous = this.port.heardIntervals();
    const intervals = this.coverage.intervals.map((interval) => ({ ...interval }));
    if (
      previous.length !== intervals.length ||
      intervals.some(
        (interval, index) =>
          interval.startMs !== previous[index]?.startMs ||
          interval.endMs !== previous[index]?.endMs,
      )
    ) {
      this.port.setHeardIntervals(intervals);
    }
  }

  accrue(now: number): void {
    if (!Number.isFinite(now) || now < 0) {
      this.lastMediaPosition = null;
      return;
    }
    if (this.lastMediaPosition !== null) {
      const delta = now - this.lastMediaPosition;
      if (delta > 0 && delta <= 1.5) {
        const origin =
          this.port.evidenceMode() && Number.isFinite(this.port.evidenceOrigin())
            ? this.port.evidenceOrigin()
            : 0;
        this.coverage = addAbsolutePlaybackInterval(
          this.coverage,
          this.lastMediaPosition,
          now,
          origin,
          this.port.evidenceLength(),
        );
        this.publish(this.coverage.uniqueMs);
      }
    }
    this.lastMediaPosition = now;
  }

  resetBaseline(): void {
    this.lastMediaPosition = null;
  }

  beginAt(position: number): void {
    this.lastMediaPosition = position;
  }

  beginAtIfEmpty(position: number): void {
    if (this.lastMediaPosition === null) this.beginAt(position);
  }

  reset(): void {
    this.coverage = emptyPlaybackCoverage();
    this.publish(0);
    this.resetBaseline();
  }

  snapshot(): readonly Readonly<PlaybackInterval>[] {
    return Object.freeze(
      this.coverage.intervals.map(({ startMs, endMs }) => Object.freeze({ startMs, endMs })),
    );
  }
}
