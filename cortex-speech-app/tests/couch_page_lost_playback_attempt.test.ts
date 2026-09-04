import { readFileSync } from 'fs';
import path from 'path';
// @ts-expect-error jsdom is already Vitest's runtime dependency but ships no bundled declarations.
import { JSDOM } from 'jsdom';
import { afterEach, describe, expect, it } from 'vitest';

/**
 * A playback attempt lives only in the server's memory (`COUCH_PLAYBACK_ATTEMPT_TTL`, 30 min, and
 * never across a restart). The phone, however, keeps the attempt id it was issued: in `<audio src>`
 * and in `playbackAttempt`. Measured 2026-09-04 on the live release: the app restarted twice in one
 * afternoon (a reboot, then a death with no exit marker) while a reviewer was mid-batch. From that
 * moment every media request for the clip on screen answered 409 "playback attempt is missing or
 * expired", the player stayed silent, and `preparePlayback` handed the SAME dead attempt back on every
 * retry — so play never recovered and Save met the same 409 at finalize, forever, until a manual
 * reload. The reviewer reads that as "no sound" and "my save does not work".
 *
 * This runs the REAL couch.html in jsdom against a scripted server and pins the recovery: a media
 * error on an attempt-carrying source re-arms ONCE through a fresh /api/playback/start; a 409 at
 * finalize re-arms and asks the reviewer to listen again instead of advancing or dropping the draft.
 */
const PAGE = path.resolve(__dirname, '..', 'src-tauri', 'assets', 'couch.html');
const CLIP = {
  id: 'lost-attempt-1',
  text: 'دەقی سەرەتایی',
  durationMs: 4000,
  rowVersion: 'rev-a',
  speakerId: null,
  pilotAfterReviewEventId: null,
};

interface Harness {
  dom: JSDOM;
  calls: string[];
  attemptIds: string[];
  finalize: { status: number };
  player: HTMLAudioElement;
  settle(): Promise<void>;
  startCalls(): number;
}

let active: Harness | null = null;

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}

async function bootPage(): Promise<Harness> {
  const calls: string[] = [];
  const attemptIds: string[] = [];
  const finalize = { status: 200 };
  let minted = 0;
  const dom = new JSDOM(readFileSync(PAGE, 'utf-8'), {
    runScripts: 'dangerously',
    url: 'http://127.0.0.1:8737/',
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    beforeParse(win: any) {
      win.HTMLCanvasElement.prototype.getContext = () => ({
        clearRect: () => {},
        fillRect: () => {},
        fillStyle: '',
      });
      if (!win.crypto || typeof win.crypto.randomUUID !== 'function') {
        Object.defineProperty(win, 'crypto', {
          configurable: true,
          value: {
            randomUUID: () => globalThis.crypto.randomUUID(),
            getRandomValues: (array: Uint8Array) => globalThis.crypto.getRandomValues(array),
          },
        });
      }
      win.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        calls.push(url.split('?')[0]);
        if (url.includes('/api/playback/start')) {
          const request = JSON.parse(String(init?.body ?? '{}'));
          minted += 1;
          const id = '20000000-0000-4000-8000-' + String(minted).padStart(12, '0');
          attemptIds.push(id);
          return json({
            playbackContractVersion: 4,
            clientAttemptId: request.clientAttemptId,
            segmentId: request.id,
            segmentRevision: request.rowVersion,
            clipDurationMs: CLIP.durationMs,
            playbackReceiptId: id,
          });
        }
        if (url.includes('/api/playback/finalize')) {
          if (finalize.status !== 200) {
            return new Response('playback attempt is missing or expired — reload this clip', {
              status: finalize.status,
              headers: { 'content-type': 'text/plain' },
            });
          }
          const request = JSON.parse(String(init?.body ?? '{}'));
          return json({
            playbackContractVersion: 4,
            playbackReceiptId: request.playbackReceiptId,
            segmentId: CLIP.id,
            segmentRevision: CLIP.rowVersion,
          });
        }
        if (url.includes('/api/decision')) return json({ ok: true });
        // /api/queue, /api/claim, /api/audio (waveform): the page must cope with these being away.
        throw new Error('offline: ' + url);
      };
    },
  });
  const settle = async () => {
    await dom.window.eval('(playbackPreparation ? playbackPreparation.promise : Promise.resolve(null))');
    for (let tick = 0; tick < 5; tick += 1) await new Promise((resolve) => setTimeout(resolve, 5));
  };
  await dom.window.eval('load()');
  dom.window.eval('queue = [' + JSON.stringify(CLIP) + ']; i = 0; exhausted = true; show(false);');
  await settle();
  const player = dom.window.document.getElementById('player') as HTMLAudioElement;
  const harness: Harness = {
    dom,
    calls,
    attemptIds,
    finalize,
    player,
    settle,
    startCalls: () => calls.filter((c) => c.endsWith('/api/playback/start')).length,
  };
  active = harness;
  return harness;
}

afterEach(() => {
  active?.dom.window.close();
  active = null;
});

describe('couch.html — a playback attempt the server has forgotten', () => {
  it('re-arms once on a media error for an attempt-carrying source, then reports a real audio failure', async () => {
    const page = await bootPage();
    expect(page.startCalls(), 'showing a clip arms it exactly once').toBe(1);
    expect(page.player.src).toContain('playbackAttemptId=' + page.attemptIds[0]);
    const warn = page.dom.window.document.getElementById('warn') as HTMLElement;

    // The server restarted: its reply to this attempt id is 409, which the element reports as `error`.
    page.player.dispatchEvent(new page.dom.window.Event('error'));
    await page.settle();
    expect(page.startCalls(), 'a dead attempt must be replaced, not reused').toBe(2);
    expect(page.player.src).toContain('playbackAttemptId=' + page.attemptIds[1]);
    expect(page.player.src).not.toContain(page.attemptIds[0]);
    expect(warn.hidden, 'one re-arm is recovery, not a broken clip').toBe(true);

    // The fresh attempt fails too: now it IS the audio, and the reviewer is told so — exactly once.
    page.player.dispatchEvent(new page.dom.window.Event('error'));
    await page.settle();
    expect(page.startCalls(), 'no re-arm loop').toBe(2);
    expect(warn.hidden).toBe(false);
    expect(warn.textContent?.trim().length ?? 0).toBeGreaterThan(0);
  });

  it('a 409 at finalize re-arms and keeps the reviewer on the clip with their correction', async () => {
    const page = await bootPage();
    const text = page.dom.window.document.getElementById('text') as HTMLTextAreaElement;
    text.value = 'دەقی ڕاستکراوە';
    page.finalize.status = 409;

    await page.dom.window.eval("decide('save')");
    await page.settle();

    expect(
      page.calls.filter((c) => c.endsWith('/api/decision')),
      'no verdict without playback authority',
    ).toHaveLength(0);
    expect(page.dom.window.eval('i'), 'the page must not advance past an unsaved clip').toBe(0);
    expect(text.value, 'the typed correction survives').toBe('دەقی ڕاستکراوە');
    expect(page.startCalls(), 'the lost attempt is replaced so the next play can succeed').toBe(2);
    expect(page.dom.window.eval('playbackAttempt && playbackAttempt.playbackReceiptId')).toBe(
      page.attemptIds[1],
    );
    expect(page.dom.window.eval('readOutbox().length'), 'nothing is queued as if it were saved').toBe(0);
  });
});
