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
 * Not a corner case: the review queue holds 361 `.mov` and 51 `.mp4` files that are one clip each, so
 * advancing nearly always switches source while the previous play() is still starting.
 */
import { render, cleanup } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import AudioPlayer from './AudioPlayer.svelte';
import { notifications, type Notification } from './stores/notificationStore';

vi.mock('./commands', () => ({
  registerMediaAsset: vi.fn(async (path: string) => ({ id: `grant-${path}`, path, expiresAt: '' })),
  getMediaAssetUrl: vi.fn(async (id: string) => `C:/cache/${id}.wav`),
}));

/** Pending play() rejecters, so pause() can abort them the way a real media element does. */
let pendingPlays: Array<(reason: unknown) => void> = [];

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
    return new Promise<void>((_resolve, reject) => {
      pendingPlays.push(reject);
    });
  };
  HTMLMediaElement.prototype.pause = function (this: HTMLMediaElement & { __paused?: boolean }) {
    this.__paused = true;
    // WHATWG: pausing rejects every pending play promise with AbortError.
    const rejecters = pendingPlays;
    pendingPlays = [];
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
    expect(
      get(notifications).length,
      'no notification of any kind for a normal advance',
    ).toBe(0);
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

    expect(errors.length, 'an undecodable clip is still an error the reviewer sees').toBeGreaterThan(
      0,
    );
  });
});
