<script lang="ts">
  import { convertFileSrc } from '@tauri-apps/api/core';
  import { getMediaAssetUrl, registerMediaAsset } from './commands';
  import { onDestroy } from 'svelte';
  import { notifications } from './stores/notificationStore';

  interface Props {
    audioPath: string;
    startTime?: number;
    endTime?: number;
    currentTime?: number;
    duration?: number;
    autoplay?: boolean;
    playing?: boolean;
  }
  let {
    audioPath,
    startTime = 0,
    endTime = 0,
    currentTime = $bindable(0),
    duration = $bindable(0),
    autoplay = false,
    playing = $bindable(false),
  }: Props = $props();
  let audioEl: HTMLAudioElement | undefined = $state();
  let loading = $state(true);
  let error = $state<string | null>(null);
  let playbackRate = $state(1.0);
  let loop = $state(false);
  const RATES = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

  function toggleRate() {
    const idx = RATES.indexOf(playbackRate);
    playbackRate = RATES[(idx + 1) % RATES.length];
    if (audioEl) audioEl.playbackRate = playbackRate;
  }

  let resolveController: AbortController | null = null;

  async function resolveAudioUrl(path: string) {
    // 1. Stop any existing playback immediately so there's no ghost audio.
    if (audioEl && !audioEl.paused) {
      audioEl.pause();
      playing = false;
    }
    // 2. Cancel any in-flight resolution for the previous path.
    resolveController?.abort();
    const ctrl = new AbortController();
    resolveController = ctrl;

    try {
      const grant = await registerMediaAsset(path);
      if (ctrl.signal.aborted) return;
      // Guard: the asset grant can be null/denied (missing file, permission,
      // or no backend) — never deref blindly or we surface a raw TypeError.
      const grantedPath = grant?.id ? await getMediaAssetUrl(grant.id) : null;
      if (ctrl.signal.aborted) return;
      if (!grantedPath) throw new Error('audio asset unavailable');
      let cleanPath = grantedPath.replaceAll('\\', '/');
      if (cleanPath.startsWith('//?/')) {
        cleanPath = cleanPath.substring(4);
      }
      // 3. Set src directly; the onloadedmetadata handler will clear loading.
      const url = convertFileSrc(cleanPath);
      if (audioEl) {
        audioEl.src = url;
        audioEl.playbackRate = playbackRate;
        audioEl.load();
      }
    } catch (e) {
      if (!ctrl.signal.aborted) {
        // Keep the technical detail in the console; show the user a clean,
        // consistent message instead of a raw "TypeError: …".
        console.error('[AudioPlayer] could not resolve audio:', e);
        error = 'Failed to load audio file';
        loading = false;
      }
    }
  }

  // Abort any pending resolution when the component is torn down.
  onDestroy(() => {
    resolveController?.abort();
    audioEl?.pause();
  });

  $effect(() => {
    if (audioPath) {
      loading = true;
      error = null;
      currentTime = 0;
      resolveAudioUrl(audioPath);
    }
  });

  // Sync playbackRate changes to the audio element reactively.
  $effect(() => {
    if (audioEl) audioEl.playbackRate = playbackRate;
  });

  $effect(() => {
    if (audioEl && Math.abs(audioEl.currentTime - currentTime) > 0.05) {
      try {
        audioEl.currentTime = currentTime;
      } catch {
        // Ignore potential errors if the audio element is not ready to seek yet
      }
    }
  });

  function reportPlaybackFailure(message: string, cause: unknown) {
    error = message;
    playing = false;
    notifications.error(message, { detail: String(cause) });
  }

  function attemptPlay(failureMessage: string) {
    if (!audioEl) return;
    audioEl
      .play()
      .then(() => (playing = true))
      .catch((e: unknown) => {
        reportPlaybackFailure(failureMessage, e);
      });
  }

  function play() {
    if (!audioEl) return;
    if (
      startTime > 0 &&
      (audioEl.currentTime < startTime || (endTime > 0 && audioEl.currentTime >= endTime))
    ) {
      audioEl.currentTime = startTime;
    }
    attemptPlay('Playback blocked or file not found');
  }

  function pause() {
    audioEl?.pause();
    playing = false;
  }

  $effect(() => {
    if (audioEl) {
      if (playing && audioEl.paused) {
        play();
      } else if (!playing && !audioEl.paused) {
        pause();
      }
    }
  });

  function handleTimeUpdate() {
    if (!audioEl) return;
    currentTime = audioEl.currentTime;
    if (endTime > 0 && audioEl.currentTime >= endTime) {
      if (loop) {
        // Respect startTime when looping a clip.
        audioEl.currentTime = startTime > 0 ? startTime : 0;
        attemptPlay('Loop playback failed');
      } else {
        audioEl.pause();
        playing = false;
      }
    }
  }

  function handleLoaded() {
    if (audioEl) {
      duration = audioEl.duration;
      loading = false;
      if (autoplay) {
        play();
      }
    }
  }

  function handleError() {
    loading = false;
    error = 'Failed to load audio file';
  }

  function seek(e: Event) {
    const target = e.currentTarget as HTMLInputElement;
    const time = parseFloat(target.value);
    if (audioEl) {
      audioEl.currentTime = time;
      currentTime = time;
    }
  }

  function fmt(s: number) {
    const m = Math.floor(s / 60);
    const sec = Math.floor(s % 60);
    return `${m}:${sec.toString().padStart(2, '0')}`;
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.code === 'Space' && e.target === audioEl) {
      e.preventDefault();
      if (playing) {
        pause();
      } else {
        play();
      }
    }
  }
</script>

<div class="flex items-center gap-3 p-3 card" role="toolbar" aria-label="Audio player controls">
  {#if loading}
    <div class="flex items-center gap-3 w-full">
      <div class="w-10 h-10 rounded-full bg-cortex-700 animate-pulse shrink-0"></div>
      <div class="flex-1 h-2 bg-cortex-700 animate-pulse rounded"></div>
      <div class="w-12 h-4 bg-cortex-700 animate-pulse rounded"></div>
    </div>
  {:else if error}
    <div class="flex items-center gap-2 text-red-300 text-xs w-full">
      <span class="text-red-400 font-bold" aria-hidden="true">!</span>
      <span>{error}</span>
      <button
        type="button"
        class="ms-auto text-xs text-cortex-400 hover:text-cortex-200"
        onclick={() => {
          error = null;
          loading = true;
          resolveAudioUrl(audioPath);
        }}>Retry</button
      >
    </div>
  {:else}
    <button
      type="button"
      class="btn btn-primary !p-2 !rounded-full"
      onclick={playing ? pause : play}
      aria-label={playing ? 'Pause' : 'Play'}
    >
      {#if playing}
        <svg class="w-5 h-5" fill="currentColor" viewBox="0 0 24 24"
          ><path d="M6 4h4v16H6V4zm8 0h4v16h-4V4z" /></svg
        >
      {:else}
        <svg class="w-5 h-5" fill="currentColor" viewBox="0 0 24 24"><path d="M8 5v14l11-7z" /></svg
        >
      {/if}
    </button>

    <span class="text-xs font-mono text-cortex-300 min-w-12">{fmt(currentTime)}</span>

    <input type="range" min="0" max={duration} value={currentTime} oninput={seek} class="flex-1" />

    <span class="text-xs font-mono text-cortex-300 min-w-12">{fmt(duration)}</span>
    <button
      type="button"
      class="btn btn-secondary !p-1.5 !px-2.5 !text-[10px] font-mono min-w-10 rounded-lg hover:bg-cortex-700/50 hover:text-default transition-colors border border-cortex-700/50 shadow-sm ms-1"
      onclick={toggleRate}
      aria-label="Playback Speed"
      title="Playback Speed"
    >
      {playbackRate}x
    </button>
    <button
      type="button"
      class="btn btn-secondary !p-1.5 !px-2.5 !text-[10px] font-mono rounded-lg hover:bg-cortex-700/50 hover:text-default transition-colors border shadow-sm ms-1 {loop
        ? 'bg-indigo-600/30 text-indigo-200 border-indigo-500/40 hover:bg-indigo-600/40'
        : 'border-cortex-700/50 text-cortex-300'}"
      onclick={() => (loop = !loop)}
      aria-label="Toggle Loop Playback"
      title="Toggle Loop Playback"
    >
      Loop {loop ? 'On' : 'Off'}
    </button>
  {/if}

  <audio
    bind:this={audioEl}
    ontimeupdate={handleTimeUpdate}
    onloadedmetadata={handleLoaded}
    onended={() => {
      if (loop) {
        if (audioEl) {
          audioEl.currentTime = startTime;
          attemptPlay('Loop playback failed');
        }
      } else {
        playing = false;
      }
    }}
    onkeydown={handleKeydown}
    onerror={handleError}
  ></audio>
</div>
