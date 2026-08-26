<script lang="ts">
  import Pause from '@lucide/svelte/icons/pause';
  import Play from '@lucide/svelte/icons/play';
  import { localMediaUrl } from './mediaSource';
  import {
    beginDesktopPlaybackSessionV1,
    cancelDesktopPlaybackSessionV1,
    getMediaAssetUrl,
    registerMediaAsset,
    registerReviewMediaAsset,
  } from './commands';
  import { onDestroy } from 'svelte';
  import { notifications } from './stores/notificationStore';
  import { t } from './i18n';
  import {
    audioAttemptBinding,
    audioTransition,
    createAudioMachine,
    isCurrentAudioAttempt,
    type AudioAttemptBinding,
    type AudioMachineEvent,
    type AudioPhase,
  } from './audioMachine';
  import {
    addAbsolutePlaybackInterval,
    emptyPlaybackCoverage,
    type PlaybackInterval,
    type PlaybackCoverage,
  } from './playbackCoverage';

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
    /** Absolute source bounds of the database segment whose receipt is being proven. Playback may
     * temporarily narrow to one word, but evidence remains relative to this immutable full span. */
    evidenceStart?: number;
    evidenceEnd?: number;
    currentTime?: number;
    duration?: number;
    autoplay?: boolean;
    /** Decision surfaces opt in. Library/curation playback must not require a review receipt. */
    requirePlaybackProof?: boolean;
    /** Review revision rendered with this clip. Authority issuance is compare-and-swap against it. */
    expectedRevision?: number;
    playing?: boolean;
    // The load/playback failure, surfaced to the PARENT so a decision surface can refuse to record a
    // human verdict on audio nobody could hear. Internal-only until 2026-08-17, when an external
    // audit found Accept/Save still enabled behind the player's own error banner: a missing-permission,
    // corrupt-container or decode-failed clip could be marked human-verified by someone who never
    // heard it. `speech_segments` has no way to distinguish that from a real listen, and this is a
    // VERBATIM corpus — the queue already refuses clips whose FILE is gone (2026-08-15); this closes
    // the same disease coming through every other failure mode.
    audioError?: string | null;
    // UNIQUE clip-relative MEDIA time this clip has actually traversed, in ms. Not wall-clock, not a
    // play() call, not a download — and not a cumulative counter that lets replaying one half twice
    // impersonate hearing the whole clip. The existing backend command accepts only a scalar today,
    // so this bound value is the exact interval-union length until policy-4 receipts carry the union.
    heardMs?: number;
    // Opaque policy-4 authority issued by the backend for this exact clip/media-grant attempt.
    // The parent finalizes the bound interval union and passes the resulting receipt into the
    // decision command; neither identity is useful for another clip or after this grant expires.
    playbackReceiptId?: string | null;
    playbackMediaGrantId?: string | null;
    /** Exact canonical duration returned by the backend session that issued playbackReceiptId. */
    playbackClipDurationMs?: number | null;
    heardIntervals?: readonly PlaybackInterval[];
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
  }: Props = $props();

  // Media-time position at the previous continuous-playback tick. Discontinuities clear this anchor;
  // the immutable union survives ordinary pause/resume and replay within the same clip.
  let lastMediaPos: number | null = null;
  let playbackCoverage: PlaybackCoverage = emptyPlaybackCoverage();
  const MAX_TICK_ADVANCE_S = 1.5;

  function publishHeardMs(value: number) {
    if (heardMs !== value) heardMs = value;
    const nextIntervals = playbackCoverage.intervals.map((interval) => ({ ...interval }));
    if (
      heardIntervals.length !== nextIntervals.length ||
      nextIntervals.some(
        (interval, index) =>
          interval.startMs !== heardIntervals[index]?.startMs ||
          interval.endMs !== heardIntervals[index]?.endMs,
      )
    ) {
      heardIntervals = nextIntervals;
    }
  }

  function accrueHeardTime(now: number) {
    if (!Number.isFinite(now) || now < 0) {
      lastMediaPos = null;
      return;
    }
    if (lastMediaPos !== null) {
      const delta = now - lastMediaPos;
      if (delta > 0 && delta <= MAX_TICK_ADVANCE_S) {
        const origin = evidenceMode && Number.isFinite(evidenceOrigin) ? evidenceOrigin : 0;
        playbackCoverage = addAbsolutePlaybackInterval(
          playbackCoverage,
          lastMediaPos,
          now,
          origin,
          evidenceLength,
        );
        publishHeardMs(playbackCoverage.uniqueMs);
      }
    }
    lastMediaPos = now;
  }

  function resetPlaybackBaseline() {
    lastMediaPos = null;
  }

  /// A new clip or newly-granted audio starts its own union: evidence never crosses clip identity.
  export function resetHeardTime() {
    playbackCoverage = emptyPlaybackCoverage();
    publishHeardMs(0);
    resetPlaybackBaseline();
  }
  let audioEl: HTMLAudioElement | undefined = $state();
  let audioMachine = createAudioMachine();
  let audioPhase = $state<AudioPhase>('idle');
  let playbackSessionPending = $state(false);
  const loading = $derived(
    playbackSessionPending ||
      audioPhase === 'idle' ||
      audioPhase === 'resolving' ||
      audioPhase === 'loading',
  );
  function transitionAudio(event: AudioMachineEvent): AudioAttemptBinding | null {
    audioMachine = audioTransition(audioMachine, event);
    audioPhase = audioMachine.phase;
    return audioAttemptBinding(audioMachine);
  }
  let playbackRate = $state(1.0);
  let loop = $state(false);
  const RATES = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

  // Browser media metadata is not trustworthy numeric input. Chromium can report `Infinity` for a
  // stream-like source and `NaN` while metadata is incomplete. Neither value may reach a range input,
  // timer, aria value or visible clock: it previously rendered `Infinity:NaN` in the review surface.
  function safeMediaSeconds(value: number): number {
    return Number.isFinite(value) && value >= 0 ? value : 0;
  }

  // Clip-relative scrubber: when bounded to a segment ([startTime, endTime]) show the CLIP
  // timeline (0:00 → clip length) instead of the whole source file, so a short sentence
  // doesn't render as a multi-minute bar you can't navigate. Internal playback still uses
  // absolute (whole-file) time; only the read-out + slider are clip-relative.
  // Display bounds fall back to the playback window when the caller doesn't override them.
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

  function toggleRate() {
    const idx = RATES.indexOf(playbackRate);
    playbackRate = RATES[(idx + 1) % RATES.length];
    if (audioEl) audioEl.playbackRate = playbackRate;
    if (playing) scheduleClipStop(); // remaining clip time changed with the rate
  }

  let resolveController: AbortController | null = null;
  let playbackSessionController: AbortController | null = null;
  let currentMediaGrantId: string | null = null;
  let mediaLoadBinding: AudioAttemptBinding | null = null;
  let activePlayBinding: AudioAttemptBinding | null = null;
  let authorityClientAttemptId: string | null = null;
  let authoritySegmentId: string | null = null;
  let authorityRevision: number | null = null;

  function retirePlaybackAuthority(receiptId: string | null, clientAttemptId: string | null) {
    if (!receiptId || !clientAttemptId) return;
    // Component teardown cannot wait for IPC. The exact receipt/attempt pair makes a late call safe,
    // and the backend independently reclaims oldest unfinalized attempts if this best-effort call is
    // lost during WebView shutdown. Finalized evidence is immutable, so that expected race is ignored.
    void cancelDesktopPlaybackSessionV1(receiptId, clientAttemptId).catch(() => undefined);
  }

  function forgetPlaybackAuthority() {
    playbackSessionController?.abort();
    playbackSessionController = null;
    playbackSessionPending = false;
    playbackReceiptId = null;
    playbackMediaGrantId = null;
    playbackClipDurationMs = null;
    authorityClientAttemptId = null;
    authoritySegmentId = null;
    authorityRevision = null;
  }

  function clearPlaybackAuthority() {
    const receiptId = playbackReceiptId;
    const clientAttemptId = authorityClientAttemptId;
    forgetPlaybackAuthority();
    retirePlaybackAuthority(receiptId, clientAttemptId);
  }

  async function beginPlaybackAuthority(
    mediaGrantId: string,
    binding: AudioAttemptBinding,
    revision: number | undefined,
    clientAttemptId: string,
  ): Promise<boolean> {
    playbackSessionController?.abort();
    const controller = new AbortController();
    playbackSessionController = controller;
    playbackSessionPending = true;
    playbackReceiptId = null;
    playbackMediaGrantId = null;
    playbackClipDurationMs = null;
    let issuedReceiptId: string | null = null;
    try {
      if (!Number.isSafeInteger(revision) || (revision ?? -1) < 0) {
        throw new Error('playback proof requires the exact rendered review revision');
      }
      const session = await beginDesktopPlaybackSessionV1(
        binding.clipId,
        mediaGrantId,
        revision as number,
        clientAttemptId,
      );
      issuedReceiptId = session.playbackReceiptId || null;
      if (
        controller.signal.aborted ||
        !isCurrentAudioAttempt(audioMachine, binding) ||
        authorityClientAttemptId !== clientAttemptId
      ) {
        retirePlaybackAuthority(issuedReceiptId, clientAttemptId);
        return false;
      }
      if (
        session.segmentId !== binding.clipId ||
        session.segmentRevision !== revision ||
        !session.playbackReceiptId ||
        !Number.isSafeInteger(session.clipDurationMs) ||
        session.clipDurationMs <= 0
      ) {
        throw new Error('playback session identity mismatch');
      }
      playbackReceiptId = session.playbackReceiptId;
      playbackMediaGrantId = mediaGrantId;
      playbackClipDurationMs = session.clipDurationMs;
      authoritySegmentId = binding.clipId;
      authorityRevision = revision as number;
      issuedReceiptId = null; // Ownership transferred to the component's current authority state.
      return true;
    } catch (error) {
      retirePlaybackAuthority(issuedReceiptId, clientAttemptId);
      if (!controller.signal.aborted && isCurrentAudioAttempt(audioMachine, binding)) {
        console.error('[AudioPlayer] could not start playback proof:', error);
        stopPhysicalPlayback();
        transitionAudio({ type: 'failed', binding, errorCode: 'PLAYBACK_PROOF_FAILED' });
        audioError = $t('audio.proofFailed');
      }
      return false;
    } finally {
      if (playbackSessionController === controller) {
        playbackSessionController = null;
        playbackSessionPending = false;
      }
    }
  }

  function stopPhysicalPlayback() {
    clearClipStop();
    if (audioEl && !audioEl.paused) audioEl.pause();
    playing = false;
    resetPlaybackBaseline();
  }

  async function resolveAudioUrl(
    path: string,
    binding: AudioAttemptBinding,
    revision: number | undefined,
    clientAttemptId: string | null,
  ) {
    // Cancel any in-flight resolution for the previous path. Its promise also carries `binding`, so
    // even a dependency that ignores AbortSignal cannot publish into the newly selected clip.
    resolveController?.abort();
    const ctrl = new AbortController();
    resolveController = ctrl;
    let issuedReceiptId: string | null = null;

    try {
      const grant = requirePlaybackProof
        ? await registerReviewMediaAsset(path)
        : await registerMediaAsset(path);
      if (ctrl.signal.aborted || !isCurrentAudioAttempt(audioMachine, binding)) return;
      // Guard: the asset grant can be null/denied (missing file, permission,
      // or no backend) — never deref blindly or we surface a raw TypeError.
      if (!grant?.id) throw new Error('audio asset unavailable');
      currentMediaGrantId = grant.id;
      if (
        requirePlaybackProof &&
        (!clientAttemptId ||
          !(await beginPlaybackAuthority(grant.id, binding, revision, clientAttemptId)))
      ) {
        return;
      }
      if (requirePlaybackProof) issuedReceiptId = playbackReceiptId;
      const grantedPath = await getMediaAssetUrl(grant.id);
      if (ctrl.signal.aborted || !isCurrentAudioAttempt(audioMachine, binding)) {
        retirePlaybackAuthority(issuedReceiptId, clientAttemptId);
        return;
      }
      if (!grantedPath) throw new Error('audio asset unavailable');
      let cleanPath = grantedPath.replaceAll('\\', '/');
      if (cleanPath.startsWith('//?/')) {
        cleanPath = cleanPath.substring(4);
      }
      transitionAudio({ type: 'resolved', binding });
      // Set src directly; the metadata/error handlers retain the attempt that owns this load.
      const url = localMediaUrl(cleanPath);
      if (!audioEl) throw new Error('audio element unavailable');
      mediaLoadBinding = binding;
      resetHeardTime(); // new source => new evidence; a previous clip's listen never carries
      audioEl.src = url;
      audioEl.playbackRate = playbackRate;
      audioEl.load();
    } catch (e) {
      retirePlaybackAuthority(issuedReceiptId, clientAttemptId);
      if (
        issuedReceiptId &&
        playbackReceiptId === issuedReceiptId &&
        authorityClientAttemptId === clientAttemptId
      ) {
        forgetPlaybackAuthority();
      }
      if (!ctrl.signal.aborted && isCurrentAudioAttempt(audioMachine, binding)) {
        // Keep the technical detail in the console; show the user a clean,
        // consistent message instead of a raw "TypeError: …".
        console.error('[AudioPlayer] could not resolve audio:', e);
        transitionAudio({ type: 'failed', binding, errorCode: 'AUDIO_RESOLUTION_FAILED' });
        audioError = $t('audio.loadFailed');
      }
    }
  }

  function retryAudio() {
    stopPhysicalPlayback();
    // A retry obtains a fresh media grant. Until the backend can bind intervals to that grant, never
    // combine evidence observed on opposite sides of the retry boundary.
    resetHeardTime();
    clearPlaybackAuthority();
    const revision = expectedRevision;
    const clientAttemptId = requirePlaybackProof ? crypto.randomUUID() : null;
    authorityClientAttemptId = clientAttemptId;
    resolveController?.abort();
    const binding = transitionAudio({ type: 'retry' });
    audioError = null;
    if (binding && audioPath) void resolveAudioUrl(audioPath, binding, revision, clientAttemptId);
  }

  /** Start a wholly new grant/session/evidence attempt after a proven non-commit finalization. */
  export function restartPlaybackAuthority() {
    retryAudio();
  }

  // Abort any pending resolution when the component is torn down.
  onDestroy(() => {
    resolveController?.abort();
    clearPlaybackAuthority();
    stopPhysicalPlayback();
    transitionAudio({ type: 'reset' });
  });

  let selectedClipMarker: string | null = null;
  let selectedSourceMarker: string | null = null;
  let selectedRevisionMarker: number | undefined = undefined;
  $effect(() => {
    const sourceId = audioPath;
    const clipId = String(clipKey ?? sourceId);
    const revision = requirePlaybackProof ? expectedRevision : undefined;
    if (
      !sourceId ||
      (clipId === selectedClipMarker &&
        sourceId === selectedSourceMarker &&
        revision === selectedRevisionMarker)
    ) {
      return;
    }

    const sourceChanged = sourceId !== selectedSourceMarker;
    selectedClipMarker = clipId;
    selectedSourceMarker = sourceId;
    selectedRevisionMarker = revision;
    stopPhysicalPlayback();
    resetHeardTime();
    clearPlaybackAuthority();
    const clientAttemptId = requirePlaybackProof ? crypto.randomUUID() : null;
    authorityClientAttemptId = clientAttemptId;
    resolveController?.abort();
    const binding = transitionAudio({ type: 'select', clipId, sourceId });
    audioError = null;
    if (sourceChanged) currentTime = 0;
    if (binding && audioMachine.phase === 'resolving') {
      void resolveAudioUrl(sourceId, binding, revision, clientAttemptId);
    } else if (binding && currentMediaGrantId && requirePlaybackProof) {
      // Same recording, different clip: the cached source can be reused, but listening authority is
      // always per clip/revision.  Autoplay waits on playbackSessionPending before it can advance.
      if (clientAttemptId) {
        void beginPlaybackAuthority(currentMediaGrantId, binding, revision, clientAttemptId);
      }
    } else if (binding) {
      void resolveAudioUrl(sourceId, binding, revision, clientAttemptId);
    }
  });

  // Autoplay each newly-selected CLIP. handleLoaded covers a fresh SOURCE load (onloadedmetadata), but
  // consecutive review segments from the SAME recording share audioPath, so the element never reloads and
  // onloadedmetadata never re-fires — without this, autoplay dies after the first clip. Key on clipKey (the
  // segment identity), not startTime, so a tap-a-word (which only narrows startTime) never re-autoplays.
  // Guarded on !loading: a different-source selection enters resolving/loading first, so this skips
  // and handleLoaded owns that autoplay — no double play. `autoplayedClip` is a plain marker,
  // so setting it here never re-triggers the effect.
  let autoplayedClip: string | null = null;
  $effect(() => {
    const marker = `${String(clipKey)}\0${requirePlaybackProof ? String(expectedRevision) : ''}`;
    if (autoplay && audioEl && !loading && clipKey !== undefined && marker !== autoplayedClip) {
      autoplayedClip = marker;
      play();
    }
  });

  // Evidence accounting is PER CLIP. Consecutive review clips often share one source recording, so
  // source identity alone cannot fence the union: key the reset on clipKey exactly like autoplay,
  // but unconditionally. The parent's bound heardMs snapshot is taken at decision time, before
  // advance, so resetting on the NEW clip's arrival never races the receipt.
  let accountedSelection: string | null = null;
  $effect(() => {
    const marker = `${String(clipKey)}\0${requirePlaybackProof ? String(expectedRevision) : ''}`;
    if (clipKey !== undefined && marker !== accountedSelection) {
      accountedSelection = marker;
      resetHeardTime();
    }
  });

  // Clip-relative coordinates are meaningful only inside one stable display/review span. ReviewMode
  // keeps this span stable while tap-a-word transiently narrows startTime/endTime; any real span or
  // duration replacement starts a fresh union rather than remapping old evidence onto new bounds.
  let accountedCoverageWindow: string | null = null;
  $effect(() => {
    const originMs = clipMode && Number.isFinite(dispStart) ? Math.round(dispStart * 1000) : 0;
    const durationMs = Math.floor(clipLength * 1000);
    const marker = `${originMs}:${durationMs}`;
    if (accountedCoverageWindow !== null && marker !== accountedCoverageWindow) resetHeardTime();
    accountedCoverageWindow = marker;
  });

  // Sync playbackRate changes to the audio element reactively.
  $effect(() => {
    if (audioEl) audioEl.playbackRate = playbackRate;
  });

  $effect(() => {
    if (audioEl && Math.abs(audioEl.currentTime - currentTime) > 0.05) {
      try {
        resetPlaybackBaseline();
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

  function reportPlaybackFailure(message: string, cause: unknown, binding: AudioAttemptBinding) {
    if (!isCurrentAudioAttempt(audioMachine, binding)) return;
    stopPhysicalPlayback();
    if ((cause as { name?: string } | null)?.name === 'NotAllowedError') {
      transitionAudio({ type: 'blocked', binding, errorCode: 'AUDIO_PLAYBACK_BLOCKED' });
    } else {
      transitionAudio({ type: 'failed', binding, errorCode: 'AUDIO_PLAYBACK_FAILED' });
    }
    audioError = message;
    activePlayBinding = null;
    notifications.error(message, { cause });
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
    const binding = activePlayBinding;
    if (!binding || !isCurrentAudioAttempt(audioMachine, binding)) return;
    const remainingSec = (endTime - audioEl.currentTime) / (playbackRate || 1);
    if (remainingSec <= 0) return;
    clipStopTimer = setTimeout(
      () => {
        clipStopTimer = null;
        // Only act if still actively playing — a timer that survived a pause or a source switch must
        // not resurrect playback (e.g. loop-restart the newly-selected clip).
        if (!audioEl || !playing || !isCurrentAudioAttempt(audioMachine, binding)) return;
        accrueHeardTime(audioEl.currentTime);
        resetPlaybackBaseline();
        if (loop) {
          audioEl.currentTime = startTime;
          attemptPlay($t('audio.loopFailed'));
        } else {
          audioEl.pause();
          playing = false;
          transitionAudio({ type: 'ended', binding });
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
  // review queue's 412 `.mov`/`.mp4` clips are spread over 140 distinct FILES (~3 clips each), so
  // advancing switches source every few clips, often while the previous play() is still starting.
  //
  function attemptPlay(failureMessage: string) {
    if (!audioEl) return;
    // Audio may not advance before its server-issued authority exists. Otherwise a fast autoplay on
    // a same-source clip can finish before the asynchronous session call returns, leaving real
    // listening that the commit boundary is correctly unable to prove.
    if (
      requirePlaybackProof &&
      (playbackSessionPending || !playbackReceiptId || !playbackMediaGrantId)
    )
      return;
    // A fresh play promise is a fresh continuity boundary. Coverage already proved for this clip is
    // retained, but no interval may bridge a pause, failed promise, loop jump or superseded attempt.
    resetPlaybackBaseline();
    const priorAttempt = audioMachine.attemptId;
    const binding = transitionAudio({ type: 'playRequested' });
    if (!binding || audioMachine.attemptId === priorAttempt) return;
    activePlayBinding = binding;
    audioEl
      .play()
      .then(() => {
        if (!isCurrentAudioAttempt(audioMachine, binding)) return;
        // Playback STARTED, so the audio is audible — clear any earlier failure. Without this the
        // error is sticky: `audioError` otherwise only clears when `audioPath` changes or the user
        // notices the Retry link, and consecutive review clips from one recording SHARE an audioPath.
        // The dominant recording holds 403 of the 414 exportable clips, so one transient failure kept
        // the reviewer's Accept/Save/Mark-bad refused for the rest of that recording.
        transitionAudio({ type: 'playStarted', binding });
        audioError = null;
        playing = true;
        if (lastMediaPos === null && audioEl && Number.isFinite(audioEl.currentTime)) {
          lastMediaPos = audioEl.currentTime;
        }
        scheduleClipStop();
      })
      .catch((e: unknown) => {
        if (!isCurrentAudioAttempt(audioMachine, binding)) return;
        // Belt and braces for an abort the element raised without a newer selection/pause attempt (a
        // `load()` from elsewhere). AbortError never means undecodable (NotSupportedError) or blocked
        // (NotAllowedError), so discarding it hides no real failure.
        if ((e as { name?: string } | null)?.name === 'AbortError') {
          transitionAudio({ type: 'pause' });
          playing = false;
          resetPlaybackBaseline();
          return;
        }
        reportPlaybackFailure(failureMessage, e, binding);
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
      resetPlaybackBaseline();
      audioEl.currentTime = startTime;
    }
    attemptPlay($t('audio.playbackFailed'));
  }

  function pause() {
    clearClipStop();
    if (audioEl && !audioEl.paused) accrueHeardTime(audioEl.currentTime);
    audioEl?.pause();
    playing = false;
    resetPlaybackBaseline();
    transitionAudio({ type: 'pause' });
  }

  /**
   * Retire physical playback and return one child-owned evidence snapshot.
   *
   * A parent cannot safely set the bound `playing` flag and immediately read its other bindings:
   * the final media delta is accrued by this component while pausing, after the parent's click
   * handler has already started. Returning the values directly from the authority owner closes that
   * race and also binds the snapshot to the exact server-issued segment revision and duration.
   */
  export function pauseAndSnapshot() {
    pause();
    const intervals = playbackCoverage.intervals.map(({ startMs, endMs }) =>
      Object.freeze({ startMs, endMs }),
    );
    return Object.freeze({
      segmentId: authoritySegmentId,
      segmentRevision: authorityRevision,
      playbackReceiptId,
      mediaGrantId: playbackMediaGrantId,
      clipDurationMs: playbackClipDurationMs,
      intervals: Object.freeze(intervals),
    });
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
    if (activePlayBinding && !isCurrentAudioAttempt(audioMachine, activePlayBinding)) return;
    currentTime = audioEl.currentTime;
    if (!audioEl.paused) accrueHeardTime(audioEl.currentTime);
    else resetPlaybackBaseline();
    // When the precise clip-stop timer is armed, let IT own the exact stop/loop. Acting here too, at
    // the ~250ms timeupdate granularity, can double-loop a short word window at the seam.
    if (clipStopTimer) return;
    if (endTime > 0 && audioEl.currentTime >= endTime) {
      if (loop) {
        // Respect startTime when looping a clip.
        resetPlaybackBaseline();
        audioEl.currentTime = startTime > 0 ? startTime : 0;
        attemptPlay($t('audio.loopFailed'));
      } else {
        audioEl.pause();
        playing = false;
        resetPlaybackBaseline();
        if (activePlayBinding) transitionAudio({ type: 'ended', binding: activePlayBinding });
      }
    }
  }

  function handleLoaded() {
    const binding = mediaLoadBinding;
    if (!audioEl || !binding || !isCurrentAudioAttempt(audioMachine, binding)) return;
    duration = safeMediaSeconds(audioEl.duration);
    transitionAudio({ type: 'loaded', binding });
    if (autoplay) {
      // Mark this clip as autoplayed so the clip-identity effect (which re-runs when loading flips
      // false) doesn't fire a second play() for the same clip.
      autoplayedClip = `${String(clipKey)}\0${
        requirePlaybackProof ? String(expectedRevision) : ''
      }`;
      play();
    }
  }

  function handleError() {
    const binding =
      activePlayBinding && isCurrentAudioAttempt(audioMachine, activePlayBinding)
        ? activePlayBinding
        : mediaLoadBinding;
    if (!binding || !isCurrentAudioAttempt(audioMachine, binding)) return;
    // A media error is terminal for this attempt. Retire its exact-stop timer and physical transport
    // before publishing `failed`; otherwise the old timer can later loop/restart the broken source or
    // overwrite failed→ended while the parent still treats the clip as unplayable.
    stopPhysicalPlayback();
    transitionAudio({ type: 'failed', binding, errorCode: 'AUDIO_DECODE_FAILED' });
    activePlayBinding = null;
    audioError = $t('audio.loadFailed');
  }

  function handleEnded() {
    const binding = activePlayBinding;
    if (!binding || !isCurrentAudioAttempt(audioMachine, binding)) return;
    if (audioEl) accrueHeardTime(audioEl.currentTime);
    resetPlaybackBaseline();
    transitionAudio({ type: 'ended', binding });
    if (loop) {
      if (audioEl) {
        audioEl.currentTime = startTime;
        attemptPlay($t('audio.loopFailed'));
      }
    } else {
      playing = false;
    }
  }

  function handleSeeking() {
    // Native controls, waveform clicks, replay and loop all pass through a media seek. Clearing the
    // anchor prevents even a sub-1.5-second seek from looking like continuous audible progression.
    resetPlaybackBaseline();
  }

  function handleSeeked() {
    if (audioEl && !audioEl.paused && Number.isFinite(audioEl.currentTime)) {
      lastMediaPos = audioEl.currentTime;
    }
  }

  function seek(e: Event) {
    const target = e.currentTarget as HTMLInputElement;
    // Slider value is display-relative when bounded; map back to absolute file time.
    const abs = clipMode ? dispStart + parseFloat(target.value) : parseFloat(target.value);
    if (audioEl) {
      resetPlaybackBaseline();
      audioEl.currentTime = abs;
      currentTime = abs;
      if (playing) scheduleClipStop(); // remaining clip time changed with the seek
    }
  }

  function fmt(s: number) {
    const bounded = safeMediaSeconds(s);
    const m = Math.floor(bounded / 60);
    const sec = Math.floor(bounded % 60);
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
        onclick={retryAudio}>{$t('retry')}</button
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
          <Pause class="h-5 w-5" strokeWidth={2.5} aria-hidden="true" />
        {:else}
          <Play class="h-5 w-5" strokeWidth={2.5} aria-hidden="true" />
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
    onended={handleEnded}
    onseeking={handleSeeking}
    onseeked={handleSeeked}
    onpause={resetPlaybackBaseline}
    onkeydown={handleKeydown}
    onerror={handleError}
  ></audio>
</div>
