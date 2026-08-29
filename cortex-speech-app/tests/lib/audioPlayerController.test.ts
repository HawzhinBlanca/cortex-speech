import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke, type InvokeArgs } from '@tauri-apps/api/core';
import { get } from 'svelte/store';
import { AudioPlayerController } from '../../src/lib/audioPlayerController';
import type { AudioPlayerInputs, AudioPlayerOutputs } from '../../src/lib/audioPlayerContract';
import type { AudioPhase } from '../../src/lib/audioMachine';
import type { PlaybackInterval } from '../../src/lib/playbackCoverage';
import { notifications } from '../../src/lib/stores/notificationStore';

const invokeMock = vi.mocked(invoke);
const GRANT_A = '00000000-0000-4000-8000-00000000000a';

interface FakeAudio {
  paused: boolean;
  currentTime: number;
  duration: number;
  playbackRate: number;
  src: string;
  play: ReturnType<typeof vi.fn<() => Promise<void>>>;
  pause: ReturnType<typeof vi.fn<() => void>>;
  load: ReturnType<typeof vi.fn<() => void>>;
}

interface HarnessState {
  audioPath: string;
  clipKey: string;
  startTime: number;
  endTime: number;
  displayStart: number;
  clipMode: boolean;
  evidenceOrigin: number;
  evidenceLength: number;
  evidenceMode: boolean;
  autoplay: boolean;
  requirePlaybackProof: boolean;
  expectedRevision: number | undefined;
  playing: boolean;
  heardMs: number;
  heardIntervals: readonly PlaybackInterval[];
  playbackReceiptId: string | null;
  playbackMediaGrantId: string | null;
  playbackClipDurationMs: number | null;
  currentTime: number;
  duration: number;
  audioError: string | null;
  audioPhase: AudioPhase;
  playbackPending: boolean;
}

function fakeAudio(): FakeAudio {
  const audio: FakeAudio = {
    paused: true,
    currentTime: 0,
    duration: 6,
    playbackRate: 1,
    src: '',
    play: vi.fn<() => Promise<void>>(),
    pause: vi.fn<() => void>(),
    load: vi.fn<() => void>(),
  };
  audio.play.mockImplementation(async () => {
    audio.paused = false;
  });
  audio.pause.mockImplementation(() => {
    audio.paused = true;
  });
  return audio;
}

function harness(overrides: Partial<HarnessState> = {}) {
  const state: HarnessState = {
    audioPath: 'C:\\private\\clip.wav',
    clipKey: 'clip-a',
    startTime: 1,
    endTime: 4,
    displayStart: 1,
    clipMode: true,
    evidenceOrigin: 1,
    evidenceLength: 3,
    evidenceMode: true,
    autoplay: false,
    requirePlaybackProof: false,
    expectedRevision: 1,
    playing: false,
    heardMs: 0,
    heardIntervals: [],
    playbackReceiptId: null,
    playbackMediaGrantId: null,
    playbackClipDurationMs: null,
    currentTime: 0,
    duration: 0,
    audioError: null,
    audioPhase: 'idle',
    playbackPending: false,
    ...overrides,
  };
  const input: AudioPlayerInputs = {
    audioPath: () => state.audioPath,
    clipKey: () => state.clipKey,
    startTime: () => state.startTime,
    endTime: () => state.endTime,
    displayStart: () => state.displayStart,
    clipMode: () => state.clipMode,
    evidenceOrigin: () => state.evidenceOrigin,
    evidenceLength: () => state.evidenceLength,
    evidenceMode: () => state.evidenceMode,
    autoplay: () => state.autoplay,
    requirePlaybackProof: () => state.requirePlaybackProof,
    expectedRevision: () => state.expectedRevision,
    playing: () => state.playing,
    heardMs: () => state.heardMs,
    heardIntervals: () => state.heardIntervals,
    playbackReceiptId: () => state.playbackReceiptId,
    playbackMediaGrantId: () => state.playbackMediaGrantId,
    playbackClipDurationMs: () => state.playbackClipDurationMs,
  };
  const output: AudioPlayerOutputs = {
    setCurrentTime: vi.fn((value) => (state.currentTime = value)),
    setDuration: vi.fn((value) => (state.duration = value)),
    setPlaying: vi.fn((value) => (state.playing = value)),
    setAudioError: vi.fn((value) => (state.audioError = value)),
    setHeardMs: vi.fn((value) => (state.heardMs = value)),
    setHeardIntervals: vi.fn((value) => (state.heardIntervals = value)),
    setPlaybackReceiptId: vi.fn((value) => (state.playbackReceiptId = value)),
    setPlaybackMediaGrantId: vi.fn((value) => (state.playbackMediaGrantId = value)),
    setPlaybackClipDurationMs: vi.fn((value) => (state.playbackClipDurationMs = value)),
    setAudioPhase: vi.fn((value) => (state.audioPhase = value)),
    setPlaybackSessionPending: vi.fn((value) => (state.playbackPending = value)),
    translate: (key) => key,
  };
  const controller = new AudioPlayerController(input, output);
  const audio = fakeAudio();
  controller.setAudioElement(audio as unknown as HTMLAudioElement);
  return { controller, state, output, audio };
}

function successfulInvoke(command: string, args?: InvokeArgs): Promise<unknown> {
  const payload = (
    args && !Array.isArray(args) && !(args instanceof ArrayBuffer) && !ArrayBuffer.isView(args)
      ? args
      : {}
  ) as Record<string, unknown>;
  switch (command) {
    case 'register_media_asset':
    case 'register_review_media_asset':
      return Promise.resolve({ id: GRANT_A, expiresAt: '2099-01-01T00:00:00Z' });
    case 'get_media_asset_url':
      return Promise.resolve(`http://cortex-media.localhost/${String(payload.id)}`);
    case 'begin_desktop_playback_session_v1':
      return Promise.resolve({
        playbackReceiptId: `receipt-${String(payload.segmentId)}`,
        segmentId: payload.segmentId,
        segmentRevision: payload.expectedRevision,
        clipDurationMs: 6000,
        expiresAtMs: Date.now() + 60_000,
      });
    case 'cancel_desktop_playback_session_v1':
      return Promise.resolve(true);
    default:
      return Promise.reject(new Error(`Unexpected command: ${command}`));
  }
}

async function loadReady(
  setup: ReturnType<typeof harness>,
  source = setup.state.audioPath,
  clip = setup.state.clipKey,
): Promise<void> {
  setup.controller.select(source, clip, setup.state.expectedRevision);
  await vi.waitFor(() => expect(setup.audio.load).toHaveBeenCalledOnce());
  setup.controller.handleLoaded();
  expect(setup.state.audioPhase).toBe('ready');
}

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockImplementation(successfulInvoke);
  notifications.clear();
  vi.spyOn(console, 'error').mockImplementation(() => {});
});

afterEach(() => {
  vi.useRealTimers();
  notifications.clear();
  vi.restoreAllMocks();
});

describe('AudioPlayerController attempt-bound lifecycle', () => {
  it('resolves a non-review asset, plays only the clip window, and snapshots exact heard intervals', async () => {
    const setup = harness();
    await loadReady(setup);

    expect(invokeMock).toHaveBeenCalledWith('register_media_asset', {
      audioPath: 'C:\\private\\clip.wav',
    });
    expect(setup.audio.src).toBe(`http://cortex-media.localhost/${GRANT_A}`);
    expect(setup.state.duration).toBe(6);

    setup.audio.currentTime = 0;
    setup.controller.play();
    await vi.waitFor(() => expect(setup.state.playing).toBe(true));
    expect(setup.audio.currentTime).toBe(1);
    expect(setup.state.audioPhase).toBe('playing');

    setup.audio.currentTime = 1.4;
    setup.controller.handleTimeUpdate();
    setup.audio.currentTime = 1.8;
    setup.controller.handleTimeUpdate();
    expect(setup.state.currentTime).toBe(1.8);
    expect(setup.state.heardMs).toBeGreaterThanOrEqual(799);
    expect(setup.state.heardMs).toBeLessThanOrEqual(800);

    const snapshot = setup.controller.pauseAndSnapshot();
    expect(snapshot).toMatchObject({
      segmentId: null,
      segmentRevision: null,
      playbackReceiptId: null,
      mediaGrantId: null,
      clipDurationMs: null,
    });
    expect(snapshot.intervals[0]?.startMs).toBe(0);
    expect(snapshot.intervals.at(-1)?.endMs).toBe(800);
    expect(
      snapshot.intervals.reduce((total, interval) => total + interval.endMs - interval.startMs, 0),
    ).toBe(setup.state.heardMs);
    expect(Object.isFrozen(snapshot)).toBe(true);
    expect(setup.state.playing).toBe(false);
    expect(setup.state.audioPhase).toBe('paused');
  });

  it('binds review playback proof to exact clip/revision and reuses only the same resolved source', async () => {
    const setup = harness({ requirePlaybackProof: true });
    await loadReady(setup);

    expect(invokeMock).toHaveBeenCalledWith('register_review_media_asset', {
      audioPath: setup.state.audioPath,
    });
    expect(invokeMock).toHaveBeenCalledWith(
      'begin_desktop_playback_session_v1',
      expect.objectContaining({
        segmentId: 'clip-a',
        mediaGrantId: GRANT_A,
        expectedRevision: 1,
      }),
    );
    expect(setup.state.playbackReceiptId).toBe('receipt-clip-a');
    expect(setup.state.playbackMediaGrantId).toBe(GRANT_A);
    expect(setup.state.playbackClipDurationMs).toBe(6000);
    expect(setup.state.playbackPending).toBe(false);

    setup.state.clipKey = 'clip-b';
    setup.state.expectedRevision = 2;
    setup.controller.select(setup.state.audioPath, 'clip-b', 2);
    await vi.waitFor(() => expect(setup.state.playbackReceiptId).toBe('receipt-clip-b'));
    expect(
      invokeMock.mock.calls.filter(([command]) => command === 'register_review_media_asset'),
    ).toHaveLength(1);
    expect(
      invokeMock.mock.calls.filter(([command]) => command === 'begin_desktop_playback_session_v1'),
    ).toHaveLength(2);
    expect(
      invokeMock.mock.calls.some(([command]) => command === 'cancel_desktop_playback_session_v1'),
    ).toBe(true);

    const calls = invokeMock.mock.calls.length;
    setup.controller.select(setup.state.audioPath, 'clip-b', 2);
    setup.controller.select('', 'clip-c', 3);
    expect(invokeMock).toHaveBeenCalledTimes(calls);
  });

  it('cancels stale resolution and turns missing grants, invalid URLs, and absent media into honest failures', async () => {
    let releaseOld!: (value: unknown) => void;
    const oldGrant = new Promise<unknown>((resolve) => (releaseOld = resolve));
    let registrations = 0;
    invokeMock.mockImplementation((command, args) => {
      if (command === 'register_media_asset') {
        registrations += 1;
        return registrations === 1
          ? oldGrant
          : Promise.resolve({ id: GRANT_A, expiresAt: 'later' });
      }
      return successfulInvoke(command, args);
    });
    const stale = harness();
    stale.controller.select('C:\\old.wav', 'old', 1);
    stale.controller.select('C:\\new.wav', 'new', 1);
    await vi.waitFor(() => expect(stale.audio.load).toHaveBeenCalledOnce());
    const authoritativeSrc = stale.audio.src;
    releaseOld({ id: '00000000-0000-4000-8000-00000000000b', expiresAt: 'later' });
    await Promise.resolve();
    expect(stale.audio.src).toBe(authoritativeSrc);
    expect(stale.state.audioError).toBeNull();

    invokeMock.mockResolvedValueOnce({ id: '', expiresAt: 'later' });
    const missing = harness();
    missing.controller.select(missing.state.audioPath, 'missing', 1);
    await vi.waitFor(() => expect(missing.state.audioPhase).toBe('failed'));
    expect(missing.state.audioError).toBe('audio.loadFailed');

    invokeMock.mockImplementation((command, args) => {
      if (command === 'get_media_asset_url') return Promise.resolve('file:///private/clip.wav');
      return successfulInvoke(command, args);
    });
    const invalidUrl = harness();
    invalidUrl.controller.select(invalidUrl.state.audioPath, 'invalid-url', 1);
    await vi.waitFor(() => expect(invalidUrl.state.audioPhase).toBe('failed'));

    invokeMock.mockImplementation(successfulInvoke);
    const absentElement = harness();
    absentElement.controller.setAudioElement(undefined);
    absentElement.controller.select(absentElement.state.audioPath, 'no-element', 1);
    await vi.waitFor(() => expect(absentElement.state.audioPhase).toBe('failed'));

    invokeMock.mockImplementation(successfulInvoke);
    missing.controller.retryAudio();
    await vi.waitFor(() => expect(missing.audio.load).toHaveBeenCalledOnce());
    const idle = harness();
    idle.controller.retryAudio();
    expect(idle.state.audioPhase).toBe('idle');
  });

  it('maps browser play refusals to blocked/failed/paused without accepting stale promises', async () => {
    const blocked = harness();
    await loadReady(blocked);
    blocked.audio.play.mockRejectedValueOnce(
      Object.assign(new Error('autoplay denied'), { name: 'NotAllowedError' }),
    );
    blocked.controller.play();
    await vi.waitFor(() => expect(blocked.state.audioPhase).toBe('blocked'));
    expect(blocked.state.audioError).toBe('audio.playbackFailed');

    const failed = harness();
    await loadReady(failed);
    failed.audio.play.mockRejectedValueOnce(new Error('decoder failed'));
    failed.controller.play();
    await vi.waitFor(() => expect(failed.state.audioPhase).toBe('failed'));

    const aborted = harness();
    await loadReady(aborted);
    const noticesBeforeAbort = get(notifications).length;
    aborted.audio.play.mockRejectedValueOnce(
      Object.assign(new Error('superseded'), { name: 'AbortError' }),
    );
    aborted.controller.play();
    await vi.waitFor(() => expect(aborted.state.audioPhase).toBe('paused'));
    expect(get(notifications)).toHaveLength(noticesBeforeAbort);

    let settlePlay!: () => void;
    const deferred = new Promise<void>((resolve) => (settlePlay = resolve));
    const stale = harness();
    await loadReady(stale);
    stale.audio.play.mockImplementationOnce(() => deferred);
    stale.controller.play();
    stale.controller.select('C:\\new.wav', 'new-clip', 1);
    settlePlay();
    await Promise.resolve();
    expect(stale.state.playing).toBe(false);
    expect(stale.state.audioPhase).toBe('resolving');
  });

  it('enforces rate, seek, sync, keyboard, selection-accounting, and decode-error edges', async () => {
    const setup = harness();
    expect(setup.controller.rate).toBe(1);
    for (const expected of [1.25, 1.5, 2, 0.5, 0.75, 1]) {
      setup.controller.toggleRate();
      expect(setup.controller.rate).toBe(expected);
      expect(setup.audio.playbackRate).toBe(expected);
    }
    setup.controller.setAudioElement(undefined);
    setup.controller.toggleRate();
    expect(setup.controller.rate).toBe(1.25);
    setup.controller.setAudioElement(setup.audio as unknown as HTMLAudioElement);

    await loadReady(setup);
    setup.controller.accountSelection('clip-a');
    setup.state.heardMs = 20;
    setup.controller.accountSelection('clip-a');
    expect(setup.state.heardMs).toBe(20);
    setup.controller.accountSelection('clip-b');
    expect(setup.state.heardMs).toBe(0);
    setup.state.heardMs = 20;
    setup.controller.accountCoverageWindow('window-a');
    expect(setup.state.heardMs).toBe(20);
    setup.controller.accountCoverageWindow('window-b');
    expect(setup.state.heardMs).toBe(0);

    setup.audio.currentTime = 1;
    setup.controller.syncCurrentTime(1.02);
    expect(setup.audio.currentTime).toBe(1);
    setup.controller.syncCurrentTime(2);
    expect(setup.audio.currentTime).toBe(2);
    setup.controller.seek({ currentTarget: { value: '0.5' } } as unknown as Event);
    expect(setup.audio.currentTime).toBe(1.5);
    expect(setup.state.currentTime).toBe(1.5);
    setup.state.clipMode = false;
    setup.controller.seek({ currentTarget: { value: '2.25' } } as unknown as Event);
    expect(setup.audio.currentTime).toBe(2.25);

    const wrongKey = new KeyboardEvent('keydown', { code: 'Enter' });
    setup.controller.handleKeydown(wrongKey);
    const space = new KeyboardEvent('keydown', { code: 'Space', cancelable: true });
    Object.defineProperty(space, 'target', { value: setup.audio });
    setup.controller.handleKeydown(space);
    await vi.waitFor(() => expect(setup.state.playing).toBe(true));
    expect(space.defaultPrevented).toBe(true);
    setup.controller.handleKeydown(space);
    expect(setup.state.playing).toBe(false);

    setup.controller.handleSeeking();
    setup.audio.paused = false;
    setup.audio.currentTime = 2;
    setup.controller.handleSeeked();
    setup.audio.currentTime = Number.NaN;
    setup.controller.handleSeeked();
    const decode = harness();
    await loadReady(decode);
    decode.controller.handleError();
    expect(decode.state.audioPhase).toBe('failed');
    expect(decode.state.audioError).toBe('audio.loadFailed');
  });

  it('stops at the exact end, loops only when requested, and ignores stale/inert media callbacks', async () => {
    const ended = harness({ startTime: 1, endTime: 1.1 });
    await loadReady(ended);
    ended.audio.currentTime = 1;
    vi.useFakeTimers();
    ended.controller.play();
    await Promise.resolve();
    expect(ended.state.playing).toBe(true);
    ended.controller.syncEndTime();
    await vi.advanceTimersByTimeAsync(101);
    expect(ended.state.playing).toBe(false);
    expect(ended.state.audioPhase).toBe('ended');
    expect(ended.audio.pause).toHaveBeenCalled();
    vi.useRealTimers();

    const looping = harness({ startTime: 2, endTime: 2.1 });
    await loadReady(looping);
    looping.controller.toggleLoop();
    expect(looping.controller.looping).toBe(true);
    looping.audio.currentTime = 2;
    vi.useFakeTimers();
    looping.controller.play();
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(101);
    expect(looping.audio.currentTime).toBe(2);
    expect(looping.audio.play.mock.calls.length).toBeGreaterThanOrEqual(2);
    vi.useRealTimers();

    looping.audio.currentTime = 2.2;
    looping.controller.handleTimeUpdate();
    looping.controller.handleEnded();
    expect(looping.audio.currentTime).toBe(2);

    const inert = harness();
    inert.controller.setAudioElement(undefined);
    inert.controller.play();
    inert.controller.pause();
    inert.controller.handleLoaded();
    inert.controller.handleTimeUpdate();
    inert.controller.handleError();
    inert.controller.handleEnded();
    inert.controller.handleSeeked();
    inert.controller.seek({ currentTarget: { value: '1' } } as unknown as Event);
    expect(inert.state.audioPhase).toBe('idle');
    inert.controller.destroy();
    expect(inert.state.audioPhase).toBe('idle');
  });
});
