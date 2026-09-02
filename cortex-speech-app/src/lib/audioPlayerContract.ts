import type { AudioPhase } from './audioMachine';
import type { TranslationKey } from './i18n';
import type { PlaybackInterval } from './playbackCoverage';

export interface AudioPlayerInputs {
  audioPath: () => string;
  clipKey: () => string | number | undefined;
  startTime: () => number;
  endTime: () => number;
  displayStart: () => number;
  clipMode: () => boolean;
  evidenceOrigin: () => number;
  evidenceLength: () => number;
  evidenceMode: () => boolean;
  autoplay: () => boolean;
  requirePlaybackProof: () => boolean;
  expectedRevision: () => number | undefined;
  playing: () => boolean;
  heardMs: () => number;
  heardIntervals: () => readonly PlaybackInterval[];
  playbackReceiptId: () => string | null;
  playbackMediaGrantId: () => string | null;
  playbackClipDurationMs: () => number | null;
}

export interface AudioPlayerOutputs {
  setCurrentTime: (value: number) => void;
  setDuration: (value: number) => void;
  setPlaying: (value: boolean) => void;
  setAudioError: (value: string | null) => void;
  setHeardMs: (value: number) => void;
  setHeardIntervals: (value: readonly PlaybackInterval[]) => void;
  setPlaybackReceiptId: (value: string | null) => void;
  setPlaybackMediaGrantId: (value: string | null) => void;
  setPlaybackClipDurationMs: (value: number | null) => void;
  setAudioPhase: (value: AudioPhase) => void;
  setPlaybackSessionPending: (value: boolean) => void;
  translate: (key: TranslationKey) => string;
}

export const safeMediaSeconds = (value: number): number =>
  Number.isFinite(value) && value >= 0 ? value : 0;
