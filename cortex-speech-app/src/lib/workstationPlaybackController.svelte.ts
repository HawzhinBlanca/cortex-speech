import { get, fromStore } from 'svelte/store';
import * as api from './commands';
import { formatPublicErrorReference } from './errorText';
import { t } from './i18n';
import { chunkPlaybackRange, parseSourceMeta, parseWordTimestamps } from './alignment';
import { notifications } from './stores/notificationStore';
import {
  segments,
  selectedSegment,
  selectedSegmentId,
  wordTimestamps,
} from './stores/segmentStore';
import type { SpeechSegment, WordTimestamp } from './types';
import { wordPlayBounds } from './wordEdit';

type PlaybackDependencies = {
  isTauriAvailable: () => boolean;
  forgetMetadata: (segmentId: string) => void;
  rememberMetadata: (
    segmentId: string,
    value: { speakerId: string | null; alignmentJson: string | null },
  ) => void;
  pruneMetadata: (ids: string[]) => void;
  retainedAutosaveIds: () => string[];
  flushAutosave: () => void;
};

export function createWorkstationPlaybackController({
  isTauriAvailable,
  forgetMetadata,
  rememberMetadata,
  pruneMetadata,
  retainedAutosaveIds,
  flushAutosave,
}: PlaybackDependencies) {
  const selectedIdStore = fromStore(selectedSegmentId);
  const selectedStore = fromStore(selectedSegment);
  let waveformData = $state<number[]>([]);
  let waveformError = $state<string | null>(null);
  let currentTime = $state(0);
  let playerDuration = $state(0);
  let isAudioPlaying = $state(false);
  let wordStartOverride = $state<number | null>(null);
  let wordEndOverride = $state<number | null>(null);
  let waveformRequest = 0;

  const chunkStartTime = $derived(
    chunkPlaybackRange(parseSourceMeta(selectedStore.current?.alignmentJson)).startTime,
  );
  const chunkEndTime = $derived(
    chunkPlaybackRange(parseSourceMeta(selectedStore.current?.alignmentJson)).endTime,
  );
  const chunkClipLength = $derived(
    chunkEndTime > chunkStartTime ? chunkEndTime - chunkStartTime : playerDuration,
  );
  const chunkClipPosition = $derived(
    chunkEndTime > chunkStartTime
      ? Math.max(0, Math.min(currentTime - chunkStartTime, chunkClipLength))
      : currentTime,
  );
  const chunkLabel = $derived.by(() => {
    const metadata = parseSourceMeta(selectedStore.current?.alignmentJson);
    return metadata && metadata.chunkCount > 1
      ? `${metadata.chunkIndex + 1} / ${metadata.chunkCount}`
      : null;
  });

  function clearWordOverride(): void {
    wordStartOverride = null;
    wordEndOverride = null;
  }

  async function loadWaveform(path: string, alignmentJson?: string | null): Promise<void> {
    const sequence = ++waveformRequest;
    if (!isTauriAvailable()) {
      waveformData = [];
      waveformError = null;
      return;
    }
    try {
      const data = await api.getWaveform(path, 200, alignmentJson);
      if (sequence !== waveformRequest) return;
      waveformData = data;
      waveformError = null;
    } catch (error) {
      if (sequence !== waveformRequest) return;
      waveformData = [];
      waveformError = formatPublicErrorReference(error) ?? get(t)('errors.unknown');
      notifications.error(get(t)('review.waveformFailed'), { cause: error });
    }
  }

  $effect(() => {
    if (!isAudioPlaying) clearWordOverride();
  });

  $effect(() => {
    const segmentId = selectedIdStore.current;
    clearWordOverride();
    if (segmentId) forgetMetadata(segmentId);
    // Depend on the selected row projection as well as its id. Full-page reloads deliberately keep
    // the selection id stable while replacing lightweight rows, so an imperative one-shot `get`
    // here would miss the replacement and leave chunk playback on null/stale alignment metadata.
    const segment = segmentId ? selectedStore.current : null;
    if (!segment) return;
    currentTime = chunkPlaybackRange(parseSourceMeta(segment.alignmentJson)).startTime;
    wordTimestamps.set(parseWordTimestamps(segment.alignmentJson));
    void loadWaveform(segment.audioPath, segment.alignmentJson);
    if (segments.isHydrated(segment.id)) {
      rememberMetadata(segment.id, {
        speakerId: segment.speakerId,
        alignmentJson: segment.alignmentJson,
      });
      pruneMetadata([segment.id, ...retainedAutosaveIds()]);
      return;
    }
    void segments
      .hydrate(segment.id)
      .then((full) => {
        if (get(selectedSegmentId) !== full.id) return;
        rememberMetadata(full.id, {
          speakerId: full.speakerId,
          alignmentJson: full.alignmentJson,
        });
        pruneMetadata([full.id, ...retainedAutosaveIds()]);
        currentTime = chunkPlaybackRange(parseSourceMeta(full.alignmentJson)).startTime;
        wordTimestamps.set(parseWordTimestamps(full.alignmentJson));
        void loadWaveform(full.audioPath, full.alignmentJson);
      })
      .catch((error) => {
        if (get(selectedSegmentId) === segment.id) {
          notifications.error(get(t)('notifications.loadSegmentsFailed'), { cause: error });
        }
      });
  });

  function playWordClip(word: WordTimestamp): void {
    const bounds = wordPlayBounds(word, chunkStartTime, chunkEndTime);
    if (isAudioPlaying && wordStartOverride === bounds.start && wordEndOverride === bounds.end) {
      return;
    }
    wordStartOverride = bounds.start;
    wordEndOverride = bounds.end;
    currentTime = bounds.start;
    isAudioPlaying = true;
  }

  function selectSegment(segment: SpeechSegment): void {
    flushAutosave();
    selectedSegmentId.set(segment.id);
  }

  function seek(time: number): void {
    clearWordOverride();
    currentTime = chunkEndTime > chunkStartTime ? chunkStartTime + time : time;
  }

  return {
    get waveformData() {
      return waveformData;
    },
    get waveformError() {
      return waveformError;
    },
    get currentTime() {
      return currentTime;
    },
    set currentTime(value: number) {
      currentTime = value;
    },
    get playerDuration() {
      return playerDuration;
    },
    set playerDuration(value: number) {
      playerDuration = value;
    },
    get isAudioPlaying() {
      return isAudioPlaying;
    },
    set isAudioPlaying(value: boolean) {
      isAudioPlaying = value;
    },
    get wordStartOverride() {
      return wordStartOverride;
    },
    get wordEndOverride() {
      return wordEndOverride;
    },
    get chunkStartTime() {
      return chunkStartTime;
    },
    get chunkEndTime() {
      return chunkEndTime;
    },
    get chunkClipLength() {
      return chunkClipLength;
    },
    get chunkClipPosition() {
      return chunkClipPosition;
    },
    get chunkLabel() {
      return chunkLabel;
    },
    clearWordOverride,
    loadWaveform,
    playWordClip,
    seek,
    selectSegment,
  };
}
