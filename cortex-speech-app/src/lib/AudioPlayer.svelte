<script lang="ts">
  import Pause from '@lucide/svelte/icons/pause';
  import Play from '@lucide/svelte/icons/play';
  import { onDestroy } from 'svelte';
  import { t } from './i18n';
  import { AudioPlayerController } from './audioPlayerController';
  import { safeMediaSeconds, type AudioPlayerInputs } from './audioPlayerContract';
  import type { AudioPhase } from './audioMachine';
  import type { PlaybackInterval } from './playbackCoverage';

  interface Props {
    audioPath: string;
    clipKey?: string | number;
    startTime?: number;
    endTime?: number;
    displayStart?: number;
    displayEnd?: number;
    evidenceStart?: number;
    evidenceEnd?: number;
    currentTime?: number;
    duration?: number;
    autoplay?: boolean;
    requirePlaybackProof?: boolean;
    expectedRevision?: number;
    playing?: boolean;
    audioError?: string | null;
    heardMs?: number;
    playbackReceiptId?: string | null;
    playbackMediaGrantId?: string | null;
    playbackClipDurationMs?: number | null;
    heardIntervals?: readonly PlaybackInterval[];
    disabled?: boolean;
    disabledDescriptionId?: string;
  }

  let {
    audioPath,
    clipKey,
    startTime = 0,
    endTime = 0,
    displayStart,
    displayEnd,
    evidenceStart,
    evidenceEnd,
    currentTime = $bindable(0),
    duration = $bindable(0),
    autoplay = false,
    requirePlaybackProof = false,
    expectedRevision,
    playing = $bindable(false),
    audioError = $bindable<string | null>(null),
    heardMs = $bindable(0),
    playbackReceiptId = $bindable<string | null>(null),
    playbackMediaGrantId = $bindable<string | null>(null),
    playbackClipDurationMs = $bindable<number | null>(null),
    heardIntervals = $bindable<readonly PlaybackInterval[]>([]),
    disabled = false,
    disabledDescriptionId,
  }: Props = $props();

  let audioEl: HTMLAudioElement | undefined = $state();
  let audioPhase = $state<AudioPhase>('idle');
  let playbackSessionPending = $state(false);
  let playbackRate = $state(1);
  let loop = $state(false);
  const loading = $derived(
    playbackSessionPending ||
      audioPhase === 'idle' ||
      audioPhase === 'resolving' ||
      audioPhase === 'loading',
  );
  const dispStart = $derived(displayStart ?? startTime);
  const dispEnd = $derived(displayEnd ?? endTime);
  const clipMode = $derived(dispEnd > dispStart);
  const clipLength = $derived(safeMediaSeconds(clipMode ? dispEnd - dispStart : duration));
  const evidenceOrigin = $derived(evidenceStart ?? dispStart);
  const evidenceLimit = $derived(evidenceEnd ?? dispEnd);
  const evidenceMode = $derived(evidenceLimit > evidenceOrigin);
  const evidenceLength = $derived(
    safeMediaSeconds(evidenceMode ? evidenceLimit - evidenceOrigin : duration),
  );
  const clipPosition = $derived(
    Math.min(safeMediaSeconds(clipMode ? currentTime - dispStart : currentTime), clipLength),
  );

  const inputs: AudioPlayerInputs = {
    audioPath: () => audioPath,
    clipKey: () => clipKey,
    startTime: () => startTime,
    endTime: () => endTime,
    displayStart: () => dispStart,
    clipMode: () => clipMode,
    evidenceOrigin: () => evidenceOrigin,
    evidenceLength: () => evidenceLength,
    evidenceMode: () => evidenceMode,
    autoplay: () => autoplay,
    requirePlaybackProof: () => requirePlaybackProof,
    expectedRevision: () => expectedRevision,
    playing: () => playing,
    heardMs: () => heardMs,
    heardIntervals: () => heardIntervals,
    playbackReceiptId: () => playbackReceiptId,
    playbackMediaGrantId: () => playbackMediaGrantId,
    playbackClipDurationMs: () => playbackClipDurationMs,
  };
  const controller = new AudioPlayerController(inputs, {
    setCurrentTime: (value) => (currentTime = value),
    setDuration: (value) => (duration = value),
    setPlaying: (value) => (playing = value),
    setAudioError: (value) => (audioError = value),
    setHeardMs: (value) => (heardMs = value),
    setHeardIntervals: (value) => (heardIntervals = value),
    setPlaybackReceiptId: (value) => (playbackReceiptId = value),
    setPlaybackMediaGrantId: (value) => (playbackMediaGrantId = value),
    setPlaybackClipDurationMs: (value) => (playbackClipDurationMs = value),
    setAudioPhase: (value) => (audioPhase = value),
    setPlaybackSessionPending: (value) => (playbackSessionPending = value),
    translate: (key) => $t(key),
  });

  $effect(() => controller.setAudioElement(audioEl));
  $effect(() => {
    const sourceId = audioPath;
    const revision = requirePlaybackProof ? expectedRevision : undefined;
    // An authoritative projection can replace the keyed player while a truth lease is still held.
    // Do not start a fresh media/playback authority in that interval; selecting on the transition
    // back to enabled issues the exact new clip/revision authority once reconciliation is complete.
    if (!disabled) controller.select(sourceId, String(clipKey ?? sourceId), revision);
  });
  $effect(() => {
    const marker = `${String(clipKey)}\0${requirePlaybackProof ? String(expectedRevision) : ''}`;
    if (!disabled && autoplay && audioEl && !loading && clipKey !== undefined)
      controller.autoplaySelection(marker);
  });
  $effect(() => {
    const marker = `${String(clipKey)}\0${requirePlaybackProof ? String(expectedRevision) : ''}`;
    if (!disabled && clipKey !== undefined) controller.accountSelection(marker);
  });
  $effect(() => {
    const originMs = clipMode && Number.isFinite(dispStart) ? Math.round(dispStart * 1000) : 0;
    controller.accountCoverageWindow(`${originMs}:${Math.floor(clipLength * 1000)}`);
  });
  $effect(() => controller.syncCurrentTime(currentTime));
  $effect(() => {
    void endTime;
    controller.syncEndTime();
  });
  $effect(() => {
    void playing;
    void disabled;
    if (disabled) {
      if (playing) playing = false;
      controller.pause();
    } else {
      controller.syncPlaying();
    }
  });
  onDestroy(() => controller.destroy());

  export function resetHeardTime() {
    controller.resetHeardTime();
  }
  export function restartPlaybackAuthority() {
    controller.retryAudio();
  }
  export function pauseAndSnapshot() {
    return controller.pauseAndSnapshot();
  }

  const formatTime = (seconds: number): string => {
    const bounded = safeMediaSeconds(seconds);
    return `${Math.floor(bounded / 60)}:${Math.floor(bounded % 60)
      .toString()
      .padStart(2, '0')}`;
  };
  const toggleRate = (): void => {
    controller.toggleRate();
    playbackRate = controller.rate;
  };
  const toggleLoop = (): void => {
    controller.toggleLoop();
    loop = controller.looping;
  };
</script>

<div
  class="flex flex-wrap items-center gap-2 p-3 card"
  role="toolbar"
  aria-label={$t('audio.controls')}
  data-testid="audio-player-controls"
  aria-disabled={disabled}
  aria-describedby={disabled ? disabledDescriptionId : undefined}
>
  {#if loading}
    <div class="flex items-center gap-3 w-full">
      <div class="w-10 h-10 rounded-full bg-cortex-700 animate-pulse shrink-0"></div>
      <div class="flex-1 h-2 bg-cortex-700 animate-pulse rounded"></div>
      <div class="w-12 h-4 bg-cortex-700 animate-pulse rounded"></div>
    </div>
  {:else if audioError}
    <div class="flex flex-wrap items-center gap-2 text-red-300 text-xs w-full">
      <span class="text-red-400 font-bold" aria-hidden="true">!</span>
      <span class="min-w-0 flex-1 basis-40 break-words">{audioError}</span>
      <button
        type="button"
        class="ms-auto shrink-0 text-xs text-cortex-400 hover:text-cortex-200"
        onclick={() => {
          if (!disabled) controller.retryAudio();
        }}
        aria-describedby={disabled ? disabledDescriptionId : undefined}
        {disabled}>{$t('retry')}</button
      >
    </div>
  {:else}
    <div
      class="flex min-w-0 flex-1 basis-52 items-center gap-2"
      data-testid="audio-player-timeline"
    >
      <button
        type="button"
        class="btn btn-primary shrink-0 !p-2 !rounded-full"
        onclick={() => {
          if (!disabled) void (playing ? controller.pause() : controller.play());
        }}
        {disabled}
        aria-describedby={disabled ? disabledDescriptionId : undefined}
        aria-label={playing ? $t('audio.pause') : $t('audio.play')}
      >
        {#if playing}
          <Pause class="h-5 w-5" strokeWidth={2.5} aria-hidden="true" />
        {:else}
          <Play class="h-5 w-5" strokeWidth={2.5} aria-hidden="true" />
        {/if}
      </button>
      <span class="shrink-0 text-xs font-mono text-cortex-300">{formatTime(clipPosition)}</span>
      <input
        type="range"
        min="0"
        max={clipLength || 0}
        value={clipPosition}
        oninput={(event) => controller.seek(event)}
        {disabled}
        aria-describedby={disabled ? disabledDescriptionId : undefined}
        class="min-w-0 flex-1"
        aria-label={$t('audio.seek')}
      />
      <span class="shrink-0 text-xs font-mono text-cortex-300">{formatTime(clipLength)}</span>
    </div>

    <div class="ms-auto flex shrink-0 items-center gap-2" data-testid="audio-player-options">
      <button
        type="button"
        class="btn btn-secondary !p-1.5 !px-2.5 !text-[10px] font-mono min-w-10 rounded-lg hover:bg-cortex-700/50 hover:text-default transition-colors border border-cortex-700/50 shadow-sm"
        onclick={toggleRate}
        {disabled}
        aria-describedby={disabled ? disabledDescriptionId : undefined}
        aria-label={$t('audio.playbackSpeed')}
        title={$t('audio.playbackSpeed')}
      >
        {playbackRate}x
      </button>
      <button
        type="button"
        class="btn btn-secondary !p-1.5 !px-2.5 !text-[10px] font-mono rounded-lg hover:bg-cortex-700/50 hover:text-default transition-colors border shadow-sm {loop
          ? 'bg-indigo-600/30 text-indigo-200 border-indigo-500/40 hover:bg-indigo-600/40'
          : 'border-cortex-700/50 text-cortex-300'}"
        onclick={toggleLoop}
        {disabled}
        aria-describedby={disabled ? disabledDescriptionId : undefined}
        aria-label={$t('audio.loopToggle')}
        title={$t('audio.loopToggle')}
      >
        {$t(loop ? 'audio.loopOn' : 'audio.loopOff')}
      </button>
    </div>
  {/if}

  <audio
    bind:this={audioEl}
    ontimeupdate={() => controller.handleTimeUpdate()}
    onloadedmetadata={() => controller.handleLoaded()}
    onended={() => controller.handleEnded()}
    onseeking={() => controller.handleSeeking()}
    onseeked={() => controller.handleSeeked()}
    onpause={() => controller.handleSeeking()}
    onkeydown={(event) => {
      if (!disabled) controller.handleKeydown(event);
    }}
    onerror={() => controller.handleError()}
  ></audio>
</div>
