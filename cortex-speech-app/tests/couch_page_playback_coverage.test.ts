import { readFileSync } from 'fs';
import path from 'path';
// @ts-expect-error jsdom is already Vitest's runtime dependency but ships no bundled declarations.
import { JSDOM } from 'jsdom';
import { afterEach, describe, expect, it } from 'vitest';

const PAGE = path.resolve(__dirname, '..', 'src-tauri', 'assets', 'couch.html');

interface RunningPage {
  dom: JSDOM;
  player: HTMLAudioElement;
  tick(seconds: number): void;
  seek(seconds: number): void;
  traversalMs(): number;
}

let active: RunningPage | null = null;

async function runningPage(clipId = 'coverage-1', durationMs = 10_000): Promise<RunningPage> {
  const dom = new JSDOM(readFileSync(PAGE, 'utf-8'), {
    runScripts: 'dangerously',
    url: 'http://127.0.0.1:8737/',
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    beforeParse(win: any) {
      win.fetch = () => Promise.reject(new Error('offline playback-coverage test'));
      win.HTMLCanvasElement.prototype.getContext = () => ({
        clearRect: () => {},
        fillRect: () => {},
        fillStyle: '',
      });
    },
  });
  await dom.window.eval('load()');
  dom.window.eval(
    `queue = [{ id: ${JSON.stringify(clipId)}, text: 'دەق', durationMs: ${durationMs}, rowVersion: 'rev-a' }]; ` +
      `i = 0; exhausted = true;
       playbackAttempt = {
         identity: playbackIdentity(queue[0]),
         clientAttemptId: '10000000-0000-4000-8000-000000000001',
         playbackReceiptId: '20000000-0000-4000-8000-000000000002',
         finalized: false,
       };
       resetPlaybackTraversalCoverage(playbackIdentity(queue[0]));
       $('player').src = proofAudioUrl(queue[0], playbackAttempt.playbackReceiptId);`,
  );

  const player = dom.window.document.getElementById('player') as HTMLAudioElement;
  Object.defineProperty(player, 'duration', {
    configurable: true,
    value: durationMs / 1000,
  });
  Object.defineProperty(player, 'paused', { configurable: true, value: false });
  player.currentTime = 0;
  player.dispatchEvent(new dom.window.Event('loadstart'));
  player.dispatchEvent(new dom.window.Event('play'));

  const page: RunningPage = {
    dom,
    player,
    tick(seconds) {
      player.currentTime = seconds;
      player.dispatchEvent(new dom.window.Event('timeupdate'));
    },
    seek(seconds) {
      player.dispatchEvent(new dom.window.Event('seeking'));
      player.currentTime = seconds;
      player.dispatchEvent(new dom.window.Event('seeked'));
    },
    traversalMs() {
      return Number(dom.window.eval('playbackTraversalMs'));
    },
  };
  active = page;
  return page;
}

afterEach(() => {
  active?.dom.window.close();
  active = null;
});

describe('couch.html unique playback coverage', () => {
  it('replaying the same half twice remains half coverage', async () => {
    const page = await runningPage();
    for (const seconds of [0.5, 1, 1.5, 2, 2.5, 3, 3.5, 4, 4.5, 5]) page.tick(seconds);
    expect(page.traversalMs()).toBe(5_000);

    page.seek(0);
    for (const seconds of [0.5, 1, 1.5, 2, 2.5, 3, 3.5, 4, 4.5, 5]) page.tick(seconds);
    expect(page.traversalMs()).toBe(5_000);
  });

  it('unions overlap and disjoint playback without counting the seek itself', async () => {
    const page = await runningPage();
    for (const seconds of [0.5, 1, 1.5, 2]) page.tick(seconds);
    page.seek(1);
    for (const seconds of [1.5, 2, 2.5, 3]) page.tick(seconds);
    page.seek(8);
    for (const seconds of [8.5, 9, 9.5, 10]) page.tick(seconds);

    expect(page.traversalMs()).toBe(5_000); // [0,3] U [8,10]
  });

  it('resets the anchor on error and all evidence on a new revision or source', async () => {
    const page = await runningPage('coverage-a');
    page.tick(0.5);
    expect(page.traversalMs()).toBe(500);

    // A media error on an attempt-carrying source RE-ARMS the clip (couch_page_lost_playback_attempt):
    // the dead attempt is replaced, and evidence gathered under it cannot be credited to the new one,
    // so the union is empty and the tick that follows the error must not start it again.
    page.player.dispatchEvent(new page.dom.window.Event('error'));
    expect(page.traversalMs(), 'a replaced attempt starts with no evidence').toBe(0);
    page.tick(1);
    expect(page.traversalMs(), 'error-to-next-tick is not continuous playback').toBe(0);

    page.dom.window.eval(
      "queue = [{ id: 'coverage-a', text: 'دەقی نوێ', durationMs: 10000, rowVersion: 'rev-b' }]; i = 0; show(false);",
    );
    expect(page.traversalMs(), 'same-id evidence is still revision-bound').toBe(0);

    page.dom.window.eval(
      "queue = [{ id: 'coverage-b', text: 'دەقی نوێ', durationMs: 10000, rowVersion: 'rev-a' }]; i = 0; show(false);",
    );
    expect(page.traversalMs()).toBe(0);
  });
});
