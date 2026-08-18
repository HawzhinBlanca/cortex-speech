<script lang="ts">
  import { convertFileSrc } from '@tauri-apps/api/core';
  import { getMediaAssetUrl, registerMediaAsset } from './commands';
  import { onDestroy } from 'svelte';
  import { notifications } from './stores/notificationStore';
  import { t } from './i18n';

  interface Props {
    audioPath: string;
    // Identity of the CLIP currently loaded (the segment id). Consecutive review clips from one
    // recording share audioPath but are distinct clips; autoplay keys on this so a same-source advance
    // re-plays even though the <audio> element never reloads. A transient tap-a-word does NOT change it.
    clipKey?: string | number;
    startTime?: number;
    endTime?: number;
    // Transport display bounds. Default to the playback window, but a caller can pass the FULL span
    // so a transient tap-a-word (which narrows startTime/endTime to ~0.4s for playback) doesn't
    // collapse the scrubber + time read-out to 0:00 on every tap.
    displayStart?: number;
    displayEnd?: number;
    currentTime?: number;
    duration?: number;
    autoplay?: boolean;
    playing?: boolean;
    // The load/playback failure, surfaced to the PARENT so a decision surface can refuse to record a
    // human verdict on audio nobody could hear. Internal-only until 2026-08-17, when an external
    // audit found Accept/Save still enabled behind the player's own error banner: a missing-permission,
    // corrupt-container or decode-failed clip could be marked human-verified by someone who never
    // heard it. `speech_segments` has no way to distinguish that from a real listen, and this is a
    // VERBATIM corpus — the queue already refuses clips whose FILE is gone (2026-08-15); this closes
    // the same disease coming through every other failure mode.
    audioError?: string | null;
  }
  let {
    audioPath,
    clipKey,
    startTime = 0,
    endTime = 0,
    displayStart,
    displayEnd,
    currentTime = $bindable(0),
    duration = $bindable(0),
    autoplay = false,
    playing = $bindable(false),
    audioError = $bindable<string | null>(null),
  }: Props = $props();
  let audioEl: HTMLAudioElement | undefined = $state();
  let loading = $state(true);
  let playbackRate = $state(1.0);
  let loop = $state(false);
  const RATES = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

  // Clip-relative scrubber: when bounded to a segment ([startTime, endTime]) show the CLIP
  // timeline (0:00 → clip length) instead of the whole source file, so a short sentence
  // doesn't render as a multi-minute bar you can't navigate. Internal playback still uses
  // absolute (whole-file) time; only the read-out + slider are clip-relative.
  // Display bounds fall back to the playback window when the caller doesn't override them.
  const dispStart = $derived(displayStart ?? startTime);
  const dispEnd = $derived(displayEnd ?? endTime);
  const clipMode = $derived(dispEnd > dispStart);
  const clipLength = $derived(clipMode ? dispEnd - dispStart : duration);
  const clipPosition = $derived(
    clipMode ? Math.max(0, Math.min(currentTime - dispStart, clipLength)) : currentTime,
  );

  function toggleRate() {
    const idx = RATES.indexOf(playbackRate);
    playbackRate = RATES[(idx + 1) % RATES.length];
    if (audioEl) audioEl.playbackRate = playbackRate;
    if (playing) scheduleClipStop(); // remaining clip time changed with the rate
  }

  let resolveController: AbortController | null = null;

  async function resolveAudioUrl(path: string) {
    // 1. Stop any existing playback immediately so there's no ghost audio. Cancel the clip-stop timer
    //    too — otherwise a pending timer from the PREVIOUS clip could fire after the source switched and
    //    (with Loop on) auto-play the newly-selected clip the user never pressed Play on.
    clearClipStop();
    supersedePlay();
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
        audioError = $t('audio.loadFailed');
        loading = false;
      }
    }
  }

  // Abort any pending resolution when the component is torn down.
  onDestroy(() => {
    clearClipStop();
    resolveController?.abort();
    audioEl?.pause();
  });

  $effect(() => {
    if (audioPath) {
      loading = true;
      audioError = null;
      currentTime = 0;
      resolveAudioUrl(audioPath);
    }
  });

  // Autoplay each newly-selected CLIP. handleLoaded covers a fresh SOURCE load (onloadedmetadata), but
  // consecutive review segments from the SAME recording share audioPath, so the element never reloads and
  // onloadedmetadata never re-fires — without this, autoplay dies after the first clip. Key on clipKey (the
  // segment identity), not startTime, so a tap-a-word (which only narrows startTime) never re-autoplays.
  // Guarded on !loading: a DIFFERENT-source advance sets loading=true in the audioPath effect above (which
  // runs first), so this skips and handleLoaded owns that autoplay — no double play. `autoplayedClip` is a
  // plain (non-reactive) marker so setting it here never re-triggers the effect.
  let autoplayedClip: string | number | undefined = undefined;
  $effect(() => {
    if (autoplay && audioEl && !loading && clipKey !== undefined && clipKey !== autoplayedClip) {
      autoplayedClip = clipKey;
      play();
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
        // A programmatic seek WHILE PLAYING (tapping a word, a parent scrubber, a loop/replay jump)
        // invalidates the clip-stop timer, which was scheduled for the OLD position's remaining time —
        // leaving the clip to stop early (seek backward) or bleed past endTime (seek forward). Reschedule
        // it from the new position so the clip still stops/loops exactly at endTime. The `> 0.05` guard
        // above means normal playback progression (the prop tracking the element) never triggers this.
        if (playing) scheduleClipStop();
      } catch {
        // Ignore potential errors if the audio element is not ready to seek yet
      }
    }
  });

  // A parent can retarget the playback window MID-PLAY (tap-a-word bounds playback to that one
  // word by overriding endTime). The clip-stop timer was scheduled against the OLD endTime, so
  // without this the word playback bleeds on to the old boundary — reschedule whenever the
  // boundary itself changes. (The seek $effect above only reschedules on currentTime jumps, which
  // misses a boundary change with an unchanged playhead, e.g. tapping the word being spoken.)
  $effect(() => {
    void endTime;
    if (playing) scheduleClipStop();
  });

  function reportPlaybackFailure(message: string, cause: unknown) {
    audioError = message;
    playing = false;
    notifications.error(message, { detail: String(cause) });
  }

  // Precise clip-boundary stop. The HTMLAudioElement `timeupdate` event only fires ~every 250ms, so
  // relying on it to stop at endTime overruns the clip by up to a quarter second — audibly bleeding the
  // first word of the NEXT segment into every clip. Schedule a setTimeout for the exact remaining clip
  // time so playback pauses (or loops) right at endTime; handleTimeUpdate stays as a coarse backstop.
  let clipStopTimer: ReturnType<typeof setTimeout> | null = null;
  function clearClipStop() {
    if (clipStopTimer) {
      clearTimeout(clipStopTimer);
      clipStopTimer = null;
    }
  }
  function scheduleClipStop() {
    clearClipStop();
    // Bail while paused: a word-length window (~0.3s) can be shorter than a cold play() promise, so a
    // timer armed here (by the seek/endTime effects, which fire before play() resolves) would pause
    // the still-starting element and reject play() with a spurious error. Every paused→playing
    // transition re-arms this via attemptPlay's .then once playback has actually begun.
    if (!audioEl || audioEl.paused || endTime <= startTime) return;
    const remainingSec = (endTime - audioEl.currentTime) / (playbackRate || 1);
    if (remainingSec <= 0) return;
    clipStopTimer = setTimeout(
      () => {
        clipStopTimer = null;
        // Only act if still actively playing — a timer that survived a pause or a source switch must
        // not resurrect playback (e.g. loop-restart the newly-selected clip).
        if (!audioEl || !playing) return;
        if (loop) {
          audioEl.currentTime = startTime;
          attemptPlay($t('audio.loopFailed'));
        } else {
          audioEl.pause();
          playing = false;
        }
      },
      Math.max(0, remainingSec * 1000),
    );
  }

  // A pending play() promise is REJECTED with AbortError whenever something supersedes it: advancing
  // to the next clip (resolveAudioUrl pauses the element and reloads it), pressing pause, or any
  // source switch. That is the app doing exactly what it was told — not audio nobody could hear.
  //
  // Reporting it was not merely noisy. `audioError` is bound to the PARENT, which disables Accept/Save
  // so a verdict cannot be recorded on a clip that could not be played (2026-08-17). So a spurious
  // AbortError LOCKED THE REVIEWER OUT of a perfectly good clip. And it is not a corner case: the
  // review queue holds 361 `.mov` and 51 `.mp4` files that are one clip each, so advancing almost
  // always switches source while the previous play() is still starting.
  //
  // `playAttempt` is the generation counter — a rejection from a superseded attempt is discarded
  // rather than reported, and only the newest attempt may set `playing`.
  let playAttempt = 0;
  function supersedePlay() {
    playAttempt += 1;
  }

  function attemptPlay(failureMessage: string) {
    if (!audioEl) return;
    const attempt = ++playAttempt;
    audioEl
      .play()
      .then(() => {
        if (attempt !== playAttempt) return; // a newer attempt owns the element now
        // Playback STARTED, so the audio is audible — clear any earlier failure. Without this the
        // error is sticky: `audioError` otherwise only clears when `audioPath` changes or the user
        // notices the Retry link, and consecutive review clips from one recording SHARE an audioPath.
        // The dominant recording holds 403 of the 414 exportable clips, so one transient failure kept
        // the reviewer's Accept/Save/Mark-bad refused for the rest of that recording.
        audioError = null;
        playing = true;
        scheduleClipStop();
      })
      .catch((e: unknown) => {
        if (attempt !== playAttempt) return;
        // Belt and braces for an abort the element raised without going through supersedePlay (a
        // `load()` from elsewhere). AbortError never means undecodable (NotSupportedError) or blocked
        // (NotAllowedError), so discarding it hides no real failure.
        if ((e as { name?: string } | null)?.name === 'AbortError') {
          playing = false;
          return;
        }
        reportPlaybackFailure(failureMessage, e);
      });
  }

  function play() {
    if (!audioEl) return;
    // Re-seek to the clip start when the playhead is outside the clip window. Guard on the clip being
    // bounded (endTime > startTime) rather than startTime > 0 — otherwise the FIRST chunk (startTime 0)
    // is never rewound and can't be replayed once it reaches its end.
    if (
      endTime > startTime &&
      (audioEl.currentTime < startTime || audioEl.currentTime >= endTime)
    ) {
      audioEl.currentTime = startTime;
    }
    attemptPlay($t('audio.playbackFailed'));
  }

  function pause() {
    clearClipStop();
    supersedePlay();
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
    // When the precise clip-stop timer is armed, let IT own the exact stop/loop. Acting here too, at
    // the ~250ms timeupdate granularity, can double-loop a short word window at the seam.
    if (clipStopTimer) return;
    if (endTime > 0 && audioEl.currentTime >= endTime) {
      if (loop) {
        // Respect startTime when looping a clip.
        audioEl.currentTime = startTime > 0 ? startTime : 0;
        attemptPlay($t('audio.loopFailed'));
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
        // Mark this clip as autoplayed so the clip-identity effect (which re-runs when loading flips
        // false) doesn't fire a second play() for the same clip.
        autoplayedClip = clipKey;
        play();
      }
    }
  }

  function handleError() {
    loading = false;
    audioError = $t('audio.loadFailed');
  }

  function seek(e: Event) {
    const target = e.currentTarget as HTMLInputElement;
    // Slider value is display-relative when bounded; map back to absolute file time.
    const abs = clipMode ? dispStart + parseFloat(target.value) : parseFloat(target.value);
    if (audioEl) {
      audioEl.currentTime = abs;
      currentTime = abs;
      if (playing) scheduleClipStop(); // remaining clip time changed with the seek
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

<div
  class="flex flex-wrap items-center gap-2 p-3 card"
  role="toolbar"
  aria-label={$t('audio.controls')}
  data-testid="audio-player-controls"
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
          audioError = null;
          loading = true;
          resolveAudioUrl(audioPath);
        }}>{$t('retry')}</button
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
        onclick={playing ? pause : play}
        aria-label={playing ? $t('audio.pause') : $t('audio.play')}
      >
        {#if playing}
          <svg class="w-5 h-5" fill="currentColor" viewBox="0 0 24 24"
            ><path d="M6 4h4v16H6V4zm8 0h4v16h-4V4z" /></svg
          >
        {:else}
          <svg class="w-5 h-5" fill="currentColor" viewBox="0 0 24 24"
            ><path d="M8 5v14l11-7z" /></svg
          >
        {/if}
      </button>

      <span class="shrink-0 text-xs font-mono text-cortex-300">{fmt(clipPosition)}</span>

      <input
        type="range"
        min="0"
        max={clipLength || 0}
        value={clipPosition}
        oninput={seek}
        class="min-w-0 flex-1"
        aria-label={$t('audio.seek')}
      />

      <span class="shrink-0 text-xs font-mono text-cortex-300">{fmt(clipLength)}</span>
    </div>

    <div class="ms-auto flex shrink-0 items-center gap-2" data-testid="audio-player-options">
      <button
        type="button"
        class="btn btn-secondary !p-1.5 !px-2.5 !text-[10px] font-mono min-w-10 rounded-lg hover:bg-cortex-700/50 hover:text-default transition-colors border border-cortex-700/50 shadow-sm"
        onclick={toggleRate}
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
        onclick={() => (loop = !loop)}
        aria-label={$t('audio.loopToggle')}
        title={$t('audio.loopToggle')}
      >
        {$t(loop ? 'audio.loopOn' : 'audio.loopOff')}
      </button>
    </div>
  {/if}

  <audio
    bind:this={audioEl}
    ontimeupdate={handleTimeUpdate}
    onloadedmetadata={handleLoaded}
    onended={() => {
      if (loop) {
        if (audioEl) {
          audioEl.currentTime = startTime;
          attemptPlay($t('audio.loopFailed'));
        }
      } else {
        playing = false;
      }
    }}
    onkeydown={handleKeydown}
    onerror={handleError}
  ></audio>
</div>
