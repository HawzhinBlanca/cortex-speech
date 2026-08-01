import { readFileSync } from 'fs';
import path from 'path';
// eslint-disable-next-line @typescript-eslint/ban-ts-comment
// @ts-expect-error jsdom ships no bundled types. It is already a devDependency (vitest's own
// environment), and pulling @types/jsdom in for one test file is not worth a dependency.
import { JSDOM } from 'jsdom';
import { describe, it, expect } from 'vitest';

/**
 * The reviewer's side of Migration v47.
 *
 * The server now sends `speakerChange` on a clip MEASURED to span a turn between two people. That is
 * only half the fix: the value is worth nothing unless the phone draws it, and no Rust test can see
 * the page. This runs the REAL `couch.html` — the same bytes `include_str!` embeds in the exe — in a
 * jsdom window, renders a clip through the page's own `show()`, and reads the resulting DOM.
 *
 * The interesting case is the third: an UNMEASURED clip must render exactly like a single-speaker
 * one. `None` means nobody looked, and a page that drew anything for it would turn absence of
 * evidence into a claim.
 *
 * It builds a real window rather than reusing vitest's ambient one because the page must run as a
 * classic SCRIPT: its top-level `let queue` then lands in the global lexical environment, where the
 * eval below can reach it. Vitest's jsdom does not execute injected scripts, and passing the source
 * to eval instead scopes those bindings to the eval — leaving `show()` reading a queue nothing can
 * assign. Both were tried; this is the one that runs the page instead of a copy of it.
 */
const PAGE = path.resolve(__dirname, '..', 'src-tauri', 'assets', 'couch.html');

type Clip = { id: string; text: string; durationMs: number; speakerId: string; speakerChange?: boolean };

/** Load the page, let it boot offline, then render `clip` through its own code path. */
function renderClip(clip: Clip): { text: string; badge: string | null } {
  const dom = new JSDOM(readFileSync(PAGE, 'utf-8'), {
    runScripts: 'dangerously',
    url: 'http://127.0.0.1:8737/',
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    beforeParse(win: any) {
      // The bootstrap fetches its queue. There is no server here and there need not be — this is
      // about rendering, not loading. A stub that never SETTLES parks the bootstrap on its await
      // rather than running an error path that would race the assertions and surface as an unhandled
      // rejection. Installed before parse, so no request is ever attempted.
      win.fetch = () => new Promise(() => {});
      // jsdom ships no canvas, so `getContext('2d')` returns null and the waveform strip that show()
      // kicks off throws. An environment gap, not a page bug — the WebView always has a 2D context —
      // so it is stubbed here rather than guarded in shipping code for a test's benefit.
      win.HTMLCanvasElement.prototype.getContext = () => ({
        clearRect: () => {},
        fillRect: () => {},
        fillStyle: '',
      });
    },
  });
  // `queue` / `i` / `show` are the page script's own top-level bindings. `exhausted` stops show()
  // from trying to refetch an empty batch. This is the page's REAL render — not a re-implementation,
  // which would keep passing while the page itself was broken.
  dom.window.eval(`queue = [${JSON.stringify(clip)}]; i = 0; exhausted = true; show(false);`);
  const meta = dom.window.document.getElementById('meta');
  const flag = meta?.querySelector('.flag') ?? null;
  const out = { text: meta?.textContent ?? '', badge: flag ? flag.textContent : null };
  dom.window.close();
  return out;
}

const BASE: Clip = { id: 'c1', text: 'دەق', durationMs: 14200, speakerId: 'SPEAKER_03' };

describe('couch.html — the speaker-change badge', () => {
  it('warns on a clip measured to hold more than one speaker', () => {
    const { text, badge } = renderClip({ ...BASE, speakerChange: true });
    expect(badge, 'a measured speaker change must be DRAWN, not merely sent').not.toBeNull();
    expect(badge).toContain('⚠');
    // The Sorani string, byte-for-byte: this page is Kurdish-first, and a badge that silently fell
    // back to English is a badge its reviewer does not read.
    expect(badge).toContain('زیاتر لە یەک قسەکەر');
    // ...without losing what the line already carried.
    expect(text).toContain('14.2s');
    expect(text).toContain('SPEAKER_03');
  });

  it('says nothing for a clip measured to hold one speaker', () => {
    const { text, badge } = renderClip({ ...BASE, speakerChange: false });
    expect(badge, 'a single-speaker clip must render as ordinary work').toBeNull();
    expect(text).toContain('SPEAKER_03');
  });

  it('says nothing for a clip nobody measured — absence of evidence is not evidence', () => {
    // No `speakerChange` field at all: a legacy row, or any future import (the import path does not
    // run the measurement). Indistinguishable from "one speaker" by design — and it must equally
    // never imply the clip WAS checked.
    const { badge } = renderClip(BASE);
    expect(badge).toBeNull();
  });
});
