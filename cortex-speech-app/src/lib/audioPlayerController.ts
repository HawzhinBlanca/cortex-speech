import { getMediaAssetUrl, registerMediaAsset, registerReviewMediaAsset } from './commands';
import { notifications } from './stores/notificationStore';
import { validatedOpaqueMediaUrl } from './opaqueMediaUrl';
import {
  audioAttemptBinding,
  audioTransition,
  createAudioMachine,
  isCurrentAudioAttempt,
  type AudioAttemptBinding,
  type AudioMachineEvent,
} from './audioMachine';
import { AudioPlaybackAuthority } from './audioPlaybackAuthority';
import { AudioPlaybackEvidence } from './audioPlaybackEvidence';
import {
  safeMediaSeconds,
  type AudioPlayerInputs,
  type AudioPlayerOutputs,
} from './audioPlayerContract';

export class AudioPlayerController {
  private audioEl: HTMLAudioElement | undefined;
  private audioMachine = createAudioMachine();
  private resolveController: AbortController | null = null;
  private currentMediaGrantId: string | null = null;
  private mediaLoadBinding: AudioAttemptBinding | null = null;
  private activePlayBinding: AudioAttemptBinding | null = null;
  private readonly authority: AudioPlaybackAuthority;
  private readonly evidence: AudioPlaybackEvidence;
  private selectedClipMarker: string | null = null;
  private selectedSourceMarker: string | null = null;
  private selectedRevisionMarker: number | undefined;
  private autoplayedClip: string | null = null;
  private accountedSelection: string | null = null;
  private accountedCoverageWindow: string | null = null;
  private clipStopTimer: ReturnType<typeof setTimeout> | null = null;
  private playbackRate = 1;
  private loop = false;

  constructor(
    private readonly input: AudioPlayerInputs,
    private readonly output: AudioPlayerOutputs,
  ) {
    this.authority = new AudioPlaybackAuthority({
      playbackReceiptId: input.playbackReceiptId,
      setPlaybackReceiptId: output.setPlaybackReceiptId,
      setPlaybackMediaGrantId: output.setPlaybackMediaGrantId,
      setPlaybackClipDurationMs: output.setPlaybackClipDurationMs,
      setPending: output.setPlaybackSessionPending,
    });
    this.evidence = new AudioPlaybackEvidence({
      heardMs: input.heardMs,
      heardIntervals: input.heardIntervals,
      evidenceMode: input.evidenceMode,
      evidenceOrigin: input.evidenceOrigin,
      evidenceLength: input.evidenceLength,
      setHeardMs: output.setHeardMs,
      setHeardIntervals: output.setHeardIntervals,
    });
  }

  setAudioElement(element: HTMLAudioElement | undefined): void {
    this.audioEl = element;
    if (element) element.playbackRate = this.playbackRate;
  }

  get rate(): number {
    return this.playbackRate;
  }

  get looping(): boolean {
    return this.loop;
  }

  toggleLoop(): void {
    this.loop = !this.loop;
  }

  toggleRate(): void {
    const rates = [0.5, 0.75, 1, 1.25, 1.5, 2];
    const index = rates.indexOf(this.playbackRate);
    this.playbackRate = rates[(index + 1) % rates.length];
    if (this.audioEl) this.audioEl.playbackRate = this.playbackRate;
    if (this.input.playing()) this.scheduleClipStop();
  }

  private transition(event: AudioMachineEvent): AudioAttemptBinding | null {
    this.audioMachine = audioTransition(this.audioMachine, event);
    this.output.setAudioPhase(this.audioMachine.phase);
    return audioAttemptBinding(this.audioMachine);
  }

  resetHeardTime(): void {
    this.evidence.reset();
  }

  private async beginPlaybackAuthority(
    mediaGrantId: string,
    binding: AudioAttemptBinding,
    revision: number | undefined,
    clientAttemptId: string,
  ): Promise<boolean> {
    return this.authority.begin(
      mediaGrantId,
      binding,
      revision,
      clientAttemptId,
      () => isCurrentAudioAttempt(this.audioMachine, binding),
      (error) => {
        console.error('[AudioPlayer] could not start playback proof:', error);
        this.stopPhysicalPlayback();
        this.transition({ type: 'failed', binding, errorCode: 'PLAYBACK_PROOF_FAILED' });
        this.output.setAudioError(this.output.translate('audio.proofFailed'));
      },
    );
  }

  private stopPhysicalPlayback(): void {
    this.clearClipStop();
    if (this.audioEl && !this.audioEl.paused) this.audioEl.pause();
    this.output.setPlaying(false);
    this.evidence.resetBaseline();
  }

  private async resolveAudioUrl(
    path: string,
    binding: AudioAttemptBinding,
    revision: number | undefined,
    clientAttemptId: string | null,
  ): Promise<void> {
    this.resolveController?.abort();
    const controller = new AbortController();
    this.resolveController = controller;
    let issuedReceiptId: string | null = null;
    try {
      const grant = this.input.requirePlaybackProof()
        ? await registerReviewMediaAsset(path)
        : await registerMediaAsset(path);
      if (controller.signal.aborted || !isCurrentAudioAttempt(this.audioMachine, binding)) return;
      if (!grant?.id) throw new Error('audio asset unavailable');
      this.currentMediaGrantId = grant.id;
      if (
        this.input.requirePlaybackProof() &&
        (!clientAttemptId ||
          !(await this.beginPlaybackAuthority(grant.id, binding, revision, clientAttemptId)))
      ) {
        return;
      }
      if (this.input.requirePlaybackProof()) issuedReceiptId = this.input.playbackReceiptId();
      const safeMediaUrl = validatedOpaqueMediaUrl(await getMediaAssetUrl(grant.id));
      if (controller.signal.aborted || !isCurrentAudioAttempt(this.audioMachine, binding)) {
        this.authority.retireIssued(issuedReceiptId, clientAttemptId);
        return;
      }
      if (!safeMediaUrl) throw new Error('audio asset unavailable');
      this.transition({ type: 'resolved', binding });
      if (!this.audioEl) throw new Error('audio element unavailable');
      this.mediaLoadBinding = binding;
      this.resetHeardTime();
      this.audioEl.src = safeMediaUrl;
      this.audioEl.playbackRate = this.playbackRate;
      this.audioEl.load();
    } catch (error) {
      this.authority.retireIssued(issuedReceiptId, clientAttemptId);
      this.authority.forgetIfCurrent(issuedReceiptId, clientAttemptId);
      if (!controller.signal.aborted && isCurrentAudioAttempt(this.audioMachine, binding)) {
        console.error('[AudioPlayer] could not resolve audio:', error);
        this.transition({ type: 'failed', binding, errorCode: 'AUDIO_RESOLUTION_FAILED' });
        this.output.setAudioError(this.output.translate('audio.loadFailed'));
      }
    }
  }

  retryAudio(): void {
    this.stopPhysicalPlayback();
    this.resetHeardTime();
    this.authority.clear();
    const revision = this.input.expectedRevision();
    const clientAttemptId = this.authority.createAttempt(this.input.requirePlaybackProof());
    this.resolveController?.abort();
    const binding = this.transition({ type: 'retry' });
    this.output.setAudioError(null);
    const path = this.input.audioPath();
    if (binding && path) void this.resolveAudioUrl(path, binding, revision, clientAttemptId);
  }

  select(sourceId: string, clipId: string, revision: number | undefined): void {
    if (
      !sourceId ||
      (clipId === this.selectedClipMarker &&
        sourceId === this.selectedSourceMarker &&
        revision === this.selectedRevisionMarker)
    ) {
      return;
    }
    const sourceChanged = sourceId !== this.selectedSourceMarker;
    this.selectedClipMarker = clipId;
    this.selectedSourceMarker = sourceId;
    this.selectedRevisionMarker = revision;
    this.stopPhysicalPlayback();
    this.resetHeardTime();
    this.authority.clear();
    const clientAttemptId = this.authority.createAttempt(this.input.requirePlaybackProof());
    this.resolveController?.abort();
    const binding = this.transition({ type: 'select', clipId, sourceId });
    this.output.setAudioError(null);
    if (sourceChanged) this.output.setCurrentTime(0);
    if (binding && this.audioMachine.phase === 'resolving') {
      void this.resolveAudioUrl(sourceId, binding, revision, clientAttemptId);
    } else if (binding && this.currentMediaGrantId && this.input.requirePlaybackProof()) {
      if (clientAttemptId) {
        void this.beginPlaybackAuthority(
          this.currentMediaGrantId,
          binding,
          revision,
          clientAttemptId,
        );
      }
    } else if (binding) {
      void this.resolveAudioUrl(sourceId, binding, revision, clientAttemptId);
    }
  }

  autoplaySelection(marker: string): void {
    if (!this.input.autoplay() || !this.audioEl || marker === this.autoplayedClip) return;
    this.autoplayedClip = marker;
    this.play();
  }

  accountSelection(marker: string): void {
    if (marker === this.accountedSelection) return;
    this.accountedSelection = marker;
    this.resetHeardTime();
  }

  accountCoverageWindow(marker: string): void {
    if (this.accountedCoverageWindow !== null && marker !== this.accountedCoverageWindow) {
      this.resetHeardTime();
    }
    this.accountedCoverageWindow = marker;
  }

  syncCurrentTime(currentTime: number): void {
    if (!this.audioEl || Math.abs(this.audioEl.currentTime - currentTime) <= 0.05) return;
    try {
      this.evidence.resetBaseline();
      this.audioEl.currentTime = currentTime;
      if (this.input.playing()) this.scheduleClipStop();
    } catch {
      // The element may not be ready to seek yet.
    }
  }

  syncEndTime(): void {
    if (this.input.playing()) this.scheduleClipStop();
  }

  syncPlaying(): void {
    if (!this.audioEl) return;
    if (this.input.playing() && this.audioEl.paused) this.play();
    else if (!this.input.playing() && !this.audioEl.paused) this.pause();
  }

  private reportPlaybackFailure(
    message: string,
    cause: unknown,
    binding: AudioAttemptBinding,
  ): void {
    if (!isCurrentAudioAttempt(this.audioMachine, binding)) return;
    this.stopPhysicalPlayback();
    this.transition({
      type: (cause as { name?: string } | null)?.name === 'NotAllowedError' ? 'blocked' : 'failed',
      binding,
      errorCode:
        (cause as { name?: string } | null)?.name === 'NotAllowedError'
          ? 'AUDIO_PLAYBACK_BLOCKED'
          : 'AUDIO_PLAYBACK_FAILED',
    });
    this.output.setAudioError(message);
    this.activePlayBinding = null;
    notifications.error(message, { cause });
  }

  private clearClipStop(): void {
    if (this.clipStopTimer) {
      clearTimeout(this.clipStopTimer);
      this.clipStopTimer = null;
    }
  }

  private scheduleClipStop(): void {
    this.clearClipStop();
    const endTime = this.input.endTime();
    const startTime = this.input.startTime();
    if (!this.audioEl || this.audioEl.paused || endTime <= startTime) return;
    const binding = this.activePlayBinding;
    if (!binding || !isCurrentAudioAttempt(this.audioMachine, binding)) return;
    const remainingSeconds = (endTime - this.audioEl.currentTime) / (this.playbackRate || 1);
    if (remainingSeconds <= 0) return;
    this.clipStopTimer = setTimeout(
      () => {
        this.clipStopTimer = null;
        if (
          !this.audioEl ||
          !this.input.playing() ||
          !isCurrentAudioAttempt(this.audioMachine, binding)
        )
          return;
        this.evidence.accrue(this.audioEl.currentTime);
        this.evidence.resetBaseline();
        if (this.loop) {
          this.audioEl.currentTime = this.input.startTime();
          this.attemptPlay(this.output.translate('audio.loopFailed'));
        } else {
          this.audioEl.pause();
          this.output.setPlaying(false);
          this.transition({ type: 'ended', binding });
        }
      },
      Math.max(0, remainingSeconds * 1000),
    );
  }

  private attemptPlay(failureMessage: string): void {
    if (!this.audioEl) return;
    if (
      this.input.requirePlaybackProof() &&
      (!this.input.playbackReceiptId() || !this.input.playbackMediaGrantId())
    ) {
      return;
    }
    this.evidence.resetBaseline();
    const priorAttempt = this.audioMachine.attemptId;
    const binding = this.transition({ type: 'playRequested' });
    if (!binding || this.audioMachine.attemptId === priorAttempt) return;
    this.activePlayBinding = binding;
    this.audioEl
      .play()
      .then(() => {
        if (!isCurrentAudioAttempt(this.audioMachine, binding)) return;
        this.transition({ type: 'playStarted', binding });
        this.output.setAudioError(null);
        this.output.setPlaying(true);
        if (this.audioEl && Number.isFinite(this.audioEl.currentTime)) {
          this.evidence.beginAtIfEmpty(this.audioEl.currentTime);
        }
        this.scheduleClipStop();
      })
      .catch((error: unknown) => {
        if (!isCurrentAudioAttempt(this.audioMachine, binding)) return;
        if ((error as { name?: string } | null)?.name === 'AbortError') {
          this.transition({ type: 'pause' });
          this.output.setPlaying(false);
          this.evidence.resetBaseline();
          return;
        }
        this.reportPlaybackFailure(failureMessage, error, binding);
      });
  }

  play(): void {
    if (!this.audioEl) return;
    const startTime = this.input.startTime();
    const endTime = this.input.endTime();
    if (
      endTime > startTime &&
      (this.audioEl.currentTime < startTime || this.audioEl.currentTime >= endTime)
    ) {
      this.evidence.resetBaseline();
      this.audioEl.currentTime = startTime;
    }
    this.attemptPlay(this.output.translate('audio.playbackFailed'));
  }

  pause(): void {
    this.clearClipStop();
    if (this.audioEl && !this.audioEl.paused) this.evidence.accrue(this.audioEl.currentTime);
    this.audioEl?.pause();
    this.output.setPlaying(false);
    this.evidence.resetBaseline();
    this.transition({ type: 'pause' });
  }

  pauseAndSnapshot() {
    this.pause();
    const intervals = this.evidence.snapshot();
    const identity = this.authority.snapshotIdentity();
    return Object.freeze({
      ...identity,
      playbackReceiptId: this.input.playbackReceiptId(),
      mediaGrantId: this.input.playbackMediaGrantId(),
      clipDurationMs: this.input.playbackClipDurationMs(),
      intervals,
    });
  }

  handleTimeUpdate(): void {
    if (!this.audioEl) return;
    if (this.activePlayBinding && !isCurrentAudioAttempt(this.audioMachine, this.activePlayBinding))
      return;
    this.output.setCurrentTime(this.audioEl.currentTime);
    if (!this.audioEl.paused) this.evidence.accrue(this.audioEl.currentTime);
    else this.evidence.resetBaseline();
    if (this.clipStopTimer) return;
    const endTime = this.input.endTime();
    if (endTime > 0 && this.audioEl.currentTime >= endTime) {
      if (this.loop) {
        this.evidence.resetBaseline();
        this.audioEl.currentTime = this.input.startTime() > 0 ? this.input.startTime() : 0;
        this.attemptPlay(this.output.translate('audio.loopFailed'));
      } else {
        this.audioEl.pause();
        this.output.setPlaying(false);
        this.evidence.resetBaseline();
        if (this.activePlayBinding)
          this.transition({ type: 'ended', binding: this.activePlayBinding });
      }
    }
  }

  handleLoaded(): void {
    const binding = this.mediaLoadBinding;
    if (!this.audioEl || !binding || !isCurrentAudioAttempt(this.audioMachine, binding)) return;
    this.output.setDuration(safeMediaSeconds(this.audioEl.duration));
    this.transition({ type: 'loaded', binding });
    if (this.input.autoplay()) {
      this.autoplayedClip = `${String(this.input.clipKey())}\0${
        this.input.requirePlaybackProof() ? String(this.input.expectedRevision()) : ''
      }`;
      this.play();
    }
  }

  handleError(): void {
    const binding =
      this.activePlayBinding && isCurrentAudioAttempt(this.audioMachine, this.activePlayBinding)
        ? this.activePlayBinding
        : this.mediaLoadBinding;
    if (!binding || !isCurrentAudioAttempt(this.audioMachine, binding)) return;
    this.stopPhysicalPlayback();
    this.transition({ type: 'failed', binding, errorCode: 'AUDIO_DECODE_FAILED' });
    this.activePlayBinding = null;
    this.output.setAudioError(this.output.translate('audio.loadFailed'));
  }

  handleEnded(): void {
    const binding = this.activePlayBinding;
    if (!binding || !isCurrentAudioAttempt(this.audioMachine, binding)) return;
    if (this.audioEl) this.evidence.accrue(this.audioEl.currentTime);
    this.evidence.resetBaseline();
    this.transition({ type: 'ended', binding });
    if (this.loop) {
      if (this.audioEl) {
        this.audioEl.currentTime = this.input.startTime();
        this.attemptPlay(this.output.translate('audio.loopFailed'));
      }
    } else {
      this.output.setPlaying(false);
    }
  }

  handleSeeking(): void {
    this.evidence.resetBaseline();
  }

  handleSeeked(): void {
    if (this.audioEl && !this.audioEl.paused && Number.isFinite(this.audioEl.currentTime)) {
      this.evidence.beginAt(this.audioEl.currentTime);
    }
  }

  seek(event: Event): void {
    const target = event.currentTarget as HTMLInputElement;
    const value = Number.parseFloat(target.value);
    const absolute = this.input.clipMode() ? this.input.displayStart() + value : value;
    if (!this.audioEl) return;
    this.evidence.resetBaseline();
    this.audioEl.currentTime = absolute;
    this.output.setCurrentTime(absolute);
    if (this.input.playing()) this.scheduleClipStop();
  }

  handleKeydown(event: KeyboardEvent): void {
    if (event.code !== 'Space' || event.target !== this.audioEl) return;
    event.preventDefault();
    if (this.input.playing()) this.pause();
    else this.play();
  }

  destroy(): void {
    this.resolveController?.abort();
    this.authority.clear();
    this.stopPhysicalPlayback();
    this.transition({ type: 'reset' });
  }
}
