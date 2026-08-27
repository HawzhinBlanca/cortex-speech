/**
 * A superseded play() must not be reported as a playback failure.
 *
 * REGRESSION 2026-08-18. `audioError` is bound to the PARENT, where it disables Accept/Save so a
 * human verdict cannot be recorded on audio nobody could hear. That guard is right — but the player
 * also reported the AbortError that the WHATWG spec raises whenever a pending play() is superseded
 * by pause()/load(). Advancing to the next clip does exactly that, so a reviewer moving briskly
 * through the queue was shown "Playback was blocked or the file could not be found" and then LOCKED
 * OUT of accepting a clip that plays perfectly.
 *
 * Not a corner case: the queue's 412 `.mov`/`.mp4` clips are spread over 140 distinct FILES (~3 clips
 * each), so advancing switches source every few clips, often while the previous play() is still
 * starting.
 */
import { render, cleanup } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import AudioPlayer from './AudioPlayer.svelte';
import AudioPlayerHost from '../../tests/fixtures/AudioPlayerHost.svelte';
import { addPlaybackInterval, emptyPlaybackCoverage } from './playbackCoverage';
import { notifications, type Notification } from './stores/notificationStore';
import * as commandApi from './commands';

vi.mock('./commands', () => ({
  registerMediaAsset: vi.fn(async () => ({
    id: '52a492d4-14d8-4e24-9f5d-bc44221b48c1',
    expiresAt: '',
  })),
  registerReviewMediaAsset: vi.fn(async () => ({
    id: '2f2d9b66-8566-4d1c-8c14-e18d006b776f',
    expiresAt: '',
  })),
  getMediaAssetUrl: vi.fn(async (id: string) => `http://cortex-media.localhost/${id}`),
  cancelDesktopPlaybackSessionV1: vi.fn(async () => true),
  beginDesktopPlaybackSessionV1: vi.fn(
    async (
      segmentId: string,
      _mediaGrantId: string,
      expectedRevision: number,
      clientAttemptId: string,
    ) => ({
      playbackReceiptId: `receipt-${clientAttemptId}`,
      segmentId,
      segmentRevision: expectedRevision,
      clipDurationMs: 10_000,
      expiresAtMs: Date.now() + 60_000,
    }),
  ),
}));

/** Pending play() settlers, so pause() can abort them the way a real media element does. */
let pendingPlays: Array<(reason: unknown) => void> = [];
let pendingPlayResolvers: Array<() => void> = [];

/** Let the newest pending play() succeed, the way a real element does once it starts. */
function resolveNewestPlay() {
  pendingPlays.pop();
  pendingPlayResolvers.pop()?.();
}

function installMediaElementStub() {
  // jsdom implements none of these; without them every play() throws "Not implemented".
  Object.defineProperty(HTMLMediaElement.prototype, 'paused', {
    configurable: true,
    get(this: HTMLMediaElement & { __paused?: boolean }) {
      return this.__paused !== false;
    },
  });
  HTMLMediaElement.prototype.play = function (this: HTMLMediaElement & { __paused?: boolean }) {
    this.__paused = false;
    // Deliberately never resolves on its own: this test is about what happens to a play() that is
    // STILL STARTING when the user advances, which is the whole race.
    return new Promise<void>((resolve, reject) => {
      pendingPlays.push(reject);
      pendingPlayResolvers.push(resolve);
    });
  };
  HTMLMediaElement.prototype.pause = function (this: HTMLMediaElement & { __paused?: boolean }) {
    this.__paused = true;
    // WHATWG: pausing rejects every pending play promise with AbortError.
    const rejecters = pendingPlays;
    pendingPlays = [];
    pendingPlayResolvers = [];
    for (const reject of rejecters) {
      reject(
        new DOMException('The play() request was interrupted by a call to pause().', 'AbortError'),
      );
    }
  };
  HTMLMediaElement.prototype.load = function () {};
}

/** Let the mocked grant promises, Svelte's effects, and the play() rejection all settle. */
async function settle() {
  for (let i = 0; i < 8; i += 1) {
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

describe('AudioPlayer: a superseded play attempt is not a playback failure', () => {
  let errors: Notification[] = [];
  let unsubscribe: () => void;

  beforeEach(() => {
    vi.clearAllMocks();
    installMediaElementStub();
    pendingPlays = [];
    pendingPlayResolvers = [];
    notifications.clear();
    errors = [];
    unsubscribe = notifications.subscribe((list) => {
      errors = list.filter((n) => n.type === 'error');
    });
  });

  afterEach(() => {
    unsubscribe?.();
    cleanup();
  });

  it('advancing to the next clip does not raise a playback error or block the decision', async () => {
    const { container, rerender } = render(AudioPlayer, {
      props: { audioPath: 'D:/queue/clip-a.mov', clipKey: 'seg-a', autoplay: true },
    });
    await settle();

    const audio = container.querySelector('audio');
    expect(audio, 'the player renders an <audio> element').toBeTruthy();
    // The element only autoplays once its metadata has loaded; jsdom never fires that itself.
    audio!.dispatchEvent(new Event('loadedmetadata'));
    await settle();
    expect(pendingPlays.length, 'autoplay started a play() that has not settled yet').toBe(1);

    // THE RACE: the reviewer advances to the next clip while that play() is still starting.
    // resolveAudioUrl pauses the element, which rejects the pending promise with AbortError.
    await rerender({ audioPath: 'D:/queue/clip-b.mov', clipKey: 'seg-b', autoplay: true });
    await settle();

    expect(
      errors.map((n) => n.message),
      'advancing mid-start must not report a playback failure',
    ).toEqual([]);
    expect(get(notifications).length, 'no notification of any kind for a normal advance').toBe(0);
  });

  it('keeps ordinary Library playback independent from review-proof authority', async () => {
    render(AudioPlayer, {
      props: { audioPath: 'D:/library/legacy-null-fingerprint.wav', clipKey: 'library-preview' },
    });
    await settle();

    expect(commandApi.registerMediaAsset).toHaveBeenCalledWith(
      'D:/library/legacy-null-fingerprint.wav',
    );
    expect(commandApi.registerReviewMediaAsset).not.toHaveBeenCalled();
    expect(commandApi.beginDesktopPlaybackSessionV1).not.toHaveBeenCalled();
  });

  it('uses only a verified grant when a review surface requires playback proof', async () => {
    render(AudioPlayer, {
      props: {
        audioPath: 'D:/review/canonical.wav',
        clipKey: 'seg-proof',
        requirePlaybackProof: true,
        expectedRevision: 0,
      },
    });
    await settle();

    expect(commandApi.registerReviewMediaAsset).toHaveBeenCalledWith('D:/review/canonical.wav');
    expect(commandApi.registerMediaAsset).not.toHaveBeenCalled();
    expect(commandApi.beginDesktopPlaybackSessionV1).toHaveBeenCalledWith(
      'seg-proof',
      '2f2d9b66-8566-4d1c-8c14-e18d006b776f',
      0,
      expect.stringMatching(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
      ),
    );
  });

  it('reissues authority and clears old evidence when only the rendered revision changes', async () => {
    const { rerender } = render(AudioPlayer, {
      props: {
        audioPath: 'D:/review/same-source.wav',
        clipKey: 'seg-same',
        requirePlaybackProof: true,
        expectedRevision: 7,
      },
    });
    await settle();

    const first = vi.mocked(commandApi.beginDesktopPlaybackSessionV1).mock.calls[0];
    expect(first?.slice(0, 3)).toEqual(['seg-same', '2f2d9b66-8566-4d1c-8c14-e18d006b776f', 7]);

    await rerender({ expectedRevision: 8 });
    await settle();

    const calls = vi.mocked(commandApi.beginDesktopPlaybackSessionV1).mock.calls;
    expect(calls).toHaveLength(2);
    expect(calls[1]?.slice(0, 3)).toEqual(['seg-same', '2f2d9b66-8566-4d1c-8c14-e18d006b776f', 8]);
    expect(calls[1]?.[3]).not.toBe(calls[0]?.[3]);
  });

  it('retires the exact authority when URL resolution fails after begin succeeds', async () => {
    vi.mocked(commandApi.getMediaAssetUrl).mockRejectedValueOnce(new Error('cache lookup failed'));
    render(AudioPlayer, {
      props: {
        audioPath: 'D:/review/url-failure.wav',
        clipKey: 'seg-url-failure',
        requirePlaybackProof: true,
        expectedRevision: 4,
      },
    });
    await settle();

    const issuance = vi.mocked(commandApi.beginDesktopPlaybackSessionV1).mock.calls[0];
    expect(issuance).toBeDefined();
    const clientAttemptId = issuance![3];
    expect(commandApi.cancelDesktopPlaybackSessionV1).toHaveBeenCalledWith(
      `receipt-${clientAttemptId}`,
      clientAttemptId,
    );
  });

  it('retires every superseded A-B-A-B-A authority with its exact client attempt', async () => {
    const { rerender, unmount } = render(AudioPlayer, {
      props: {
        audioPath: 'D:/review/shared.wav',
        clipKey: 'seg-a',
        requirePlaybackProof: true,
        expectedRevision: 0,
      },
    });
    await settle();
    for (const clipKey of ['seg-b', 'seg-a', 'seg-b', 'seg-a']) {
      await rerender({ clipKey });
      await settle();
    }
    unmount();
    await settle();

    const issuanceCalls = vi.mocked(commandApi.beginDesktopPlaybackSessionV1).mock.calls;
    expect(issuanceCalls).toHaveLength(5);
    expect(commandApi.cancelDesktopPlaybackSessionV1).toHaveBeenCalledTimes(5);
    for (const issuance of issuanceCalls) {
      const clientAttemptId = issuance[3];
      expect(commandApi.cancelDesktopPlaybackSessionV1).toHaveBeenCalledWith(
        `receipt-${clientAttemptId}`,
        clientAttemptId,
      );
    }
  });

  it('a genuinely undecodable clip is STILL reported', async () => {
    // The guard must not swallow real failures. A clip the WebView cannot decode rejects with
    // NotSupportedError, which nothing superseded — the reviewer has to see that one.
    const { container } = render(AudioPlayer, {
      props: { audioPath: 'D:/queue/broken.mov', clipKey: 'seg-broken', autoplay: true },
    });
    await settle();
    container.querySelector('audio')!.dispatchEvent(new Event('loadedmetadata'));
    await settle();
    expect(pendingPlays.length).toBe(1);

    pendingPlays.pop()!(
      new DOMException('The element has no supported sources.', 'NotSupportedError'),
    );
    await settle();

    expect(
      errors.length,
      'an undecodable clip is still an error the reviewer sees',
    ).toBeGreaterThan(0);
  });

  it('a media error retires an armed loop timer and remains terminal', async () => {
    const { container } = render(AudioPlayer, {
      props: {
        audioPath: 'D:/queue/decodes-then-fails.wav',
        clipKey: 'seg-terminal-error',
        autoplay: true,
        startTime: 0,
        endTime: 0.04,
      },
    });
    await settle();
    const audio = container.querySelector('audio')!;
    audio.dispatchEvent(new Event('loadedmetadata'));
    await settle();
    resolveNewestPlay();
    await settle();

    const loopButton = container.querySelector<HTMLButtonElement>(
      '[data-testid="audio-player-options"] button:last-child',
    );
    expect(loopButton).not.toBeNull();
    loopButton!.click();
    audio.dispatchEvent(new Event('error'));
    await settle();
    await new Promise((resolve) => setTimeout(resolve, 80));

    expect(audio.paused).toBe(true);
    expect(pendingPlays, 'the retired clip-stop timer must not restart broken media').toHaveLength(
      0,
    );
    expect(container.querySelector('[data-testid="audio-player-timeline"]')).toBeNull();
  });

  it('never renders non-finite media metadata into the clock or seek range', async () => {
    const { container } = render(AudioPlayer, {
      props: { audioPath: 'D:/queue/stream-like.wav', clipKey: 'seg-stream' },
    });
    await settle();

    const audio = container.querySelector('audio')!;
    Object.defineProperty(audio, 'duration', { configurable: true, value: Infinity });
    audio.dispatchEvent(new Event('loadedmetadata'));
    await settle();

    const timeline = container.querySelector('[data-testid="audio-player-timeline"]');
    expect(timeline, 'loaded media exposes the transport').not.toBeNull();
    expect(timeline?.textContent).not.toMatch(/Infinity|NaN/);
    expect(timeline?.querySelector('input[type="range"]')).toHaveAttribute('max', '0');
  });

  it('the next clip of the SAME recording is not still blocked by the previous failure', async () => {
    // `audioError` is bound to the parent, where it refuses Accept/Save/Mark-bad. It cleared only
    // when `audioPath` CHANGED (or if the reviewer spotted the Retry link) — but consecutive review
    // clips from one recording SHARE an audioPath, and while the banner is up there is no play button
    // to press. The dominant recording holds 403 of the 414 exportable clips (and 147 queue clips per
    // .wav file on average), so one failure left the reviewer refused for the rest of that recording.
    const { container, rerender } = render(AudioPlayerHost, {
      props: { path: 'D:/queue/one-recording.flac', clipKey: 'clip-1' },
    });
    await settle();
    container.querySelector('audio')!.dispatchEvent(new Event('loadedmetadata'));
    await settle();

    pendingPlays.pop()!(new DOMException('transient', 'NotSupportedError'));
    pendingPlayResolvers.pop();
    await settle();
    expect(
      container.querySelector('[data-testid="audio-player-timeline"]'),
      'the error banner replaces the transport, and there is no play button behind it',
    ).toBeNull();

    // Advance to the NEXT clip of the same recording: audioPath is unchanged, so nothing reloads —
    // only autoplay fires, and this time playback starts.
    await rerender({ clipKey: 'clip-2' });
    await settle();
    resolveNewestPlay();
    await settle();

    expect(
      container.querySelector('[data-testid="audio-player-timeline"]'),
      'playback started on the next clip, so the audio is audible and the decision must unblock',
    ).not.toBeNull();
  });
});

describe('unique media-time accounting (playback evidence)', () => {
  // `audioError` proved the absence of a FAILURE, never the presence of listening. These pin the
  // measure that replaces it: media time actually advanced.
  function player() {
    // A minimal stand-in: `paused` is read-only on the real element, so the accounting rules are
    // exercised against a writable shape rather than a live media element.
    const el = { currentTime: 0, paused: false };
    let coverage = emptyPlaybackCoverage();
    let last: number | null = null;
    const MAX = 1.5;
    const CLIP_MS = 10_000;
    return {
      el,
      get heardMs() {
        return coverage.uniqueMs;
      },
      reset() {
        coverage = emptyPlaybackCoverage();
        last = null;
      },
      tick() {
        if (el.paused) {
          last = null;
          return;
        }
        if (last !== null) {
          const d = el.currentTime - last;
          if (d > 0 && d <= MAX) {
            coverage = addPlaybackInterval(coverage, last * 1000, el.currentTime * 1000, CLIP_MS);
          }
        }
        last = el.currentTime;
      },
    };
  }

  it('counts forward playback', () => {
    const p = player();
    for (const t of [0, 0.25, 0.5, 0.75, 1.0]) {
      p.el.currentTime = t;
      p.tick();
    }
    expect(Math.round(p.heardMs)).toBe(1000);
  });

  it('does not count a seek as listening', () => {
    const p = player();
    p.el.currentTime = 0;
    p.tick();
    p.el.currentTime = 8; // scrubbed to the end
    p.tick();
    expect(p.heardMs).toBe(0);
  });

  it('does not count time while paused', () => {
    const p = player();
    p.el.currentTime = 0;
    p.tick();
    p.el.paused = true;
    p.el.currentTime = 5;
    p.tick();
    expect(p.heardMs).toBe(0);
  });

  it('replaying the same half never adds up to the whole clip', () => {
    const p = player();
    for (const t of [0, 0.5, 1.0]) {
      p.el.currentTime = t;
      p.tick();
    }
    p.el.currentTime = 0; // back to the start
    p.tick();
    for (const t of [0.5, 1.0]) {
      p.el.currentTime = t;
      p.tick();
    }
    expect(Math.round(p.heardMs)).toBe(1000);
  });

  it('a new source resets the evidence', () => {
    const p = player();
    p.el.currentTime = 0;
    p.tick();
    p.el.currentTime = 1;
    p.tick();
    expect(p.heardMs).toBeGreaterThan(0);
    p.reset();
    expect(p.heardMs).toBe(0);
  });
});
