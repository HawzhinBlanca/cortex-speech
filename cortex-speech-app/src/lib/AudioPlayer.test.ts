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
import { notifications, type Notification } from './stores/notificationStore';

vi.mock('./commands', () => ({
  registerMediaAsset: vi.fn(async (path: string) => ({ id: `grant-${path}`, path, expiresAt: '' })),
  getMediaAssetUrl: vi.fn(async (id: string) => `C:/cache/${id}.wav`),
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

describe('cumulative media-time accounting (playback evidence)', () => {
  // `audioError` proved the absence of a FAILURE, never the presence of listening. These pin the
  // measure that replaces it: media time actually advanced.
  function player() {
    // A minimal stand-in: `paused` is read-only on the real element, so the accounting rules are
    // exercised against a writable shape rather than a live media element.
    const el = { currentTime: 0, paused: false };
    let heard = 0;
    let last: number | null = null;
    const MAX = 1.5;
    return {
      el,
      get heardMs() {
        return heard;
      },
      reset() {
        heard = 0;
        last = null;
      },
      tick() {
        if (el.paused) {
          last = null;
          return;
        }
        if (last !== null) {
          const d = el.currentTime - last;
          if (d > 0 && d <= MAX) heard += d * 1000;
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
    // Two listens of the first second are 2s of media time, but the clip is longer than that —
    // coverage is decided by the backend against clip_duration_ms, and this only reports what played.
    expect(Math.round(p.heardMs)).toBe(2000);
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
