import { readFileSync } from 'fs';
import path from 'path';
// @ts-expect-error jsdom ships no bundled types (see couch_page_speaker_change_badge.test.ts).
import { JSDOM } from 'jsdom';
import { describe, it, expect } from 'vitest';

/**
 * Item 1 (owner, 2026-09-07): the server sends `uncertainWords` — the aligner's least-certain words of
 * the served draft — so the reviewer's ear goes to them first. Like the speaker badge, the value is
 * worth nothing unless the phone DRAWS it, and no Rust test can see the page. This runs the REAL
 * `couch.html` in jsdom, renders through the page's own `show()`, and reads the DOM.
 *
 * The third case matters most: a clip WITHOUT the field must render exactly like before. Absent means
 * the clip was never aligned, and a page that drew anything for it would turn absence of evidence into
 * a claim about the draft.
 */
const PAGE = path.resolve(__dirname, '..', 'src-tauri', 'assets', 'couch.html');

type Clip = {
  id: string;
  text: string;
  durationMs: number;
  speakerId: string;
  uncertainWords?: string[] | null;
};

async function renderClip(clip: Clip): Promise<{ meta: string; hint: string | null }> {
  const dom = new JSDOM(readFileSync(PAGE, 'utf-8'), {
    runScripts: 'dangerously',
    url: 'http://127.0.0.1:8737/',
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    beforeParse(win: any) {
      win.fetch = () => Promise.reject(new Error('offline uncertain-words test'));
      win.HTMLCanvasElement.prototype.getContext = () => ({
        clearRect: () => {},
        fillRect: () => {},
        fillStyle: '',
      });
    },
  });
  await dom.window.eval('load()');
  dom.window.eval(`queue = [${JSON.stringify(clip)}]; i = 0; exhausted = true; show(false);`);
  const meta = dom.window.document.getElementById('meta');
  const hint = meta?.querySelector('.uncertain') ?? null;
  const out = { meta: meta?.textContent ?? '', hint: hint ? hint.textContent : null };
  dom.window.close();
  return out;
}

const BASE: Clip = { id: 'c1', text: 'دەقی چامپیۆن', durationMs: 1000, speakerId: 'Lamo' };

describe('couch.html — the uncertain-words hint', () => {
  it('names the words the aligner was least sure of, in Sorani by default', async () => {
    const { hint } = await renderClip({ ...BASE, uncertainWords: ['چامپیۆن', 'دەقی'] });
    expect(hint, 'the hint must be DRAWN, not merely sent').not.toBeNull();
    expect(hint).toContain('وشە نادڵنیاکان');
    expect(hint).toContain('چامپیۆن، دەقی');
  });

  it('is appended as a text node, never interpreted as markup', async () => {
    const { hint } = await renderClip({ ...BASE, uncertainWords: ['<b>x</b>'] });
    expect(hint).toContain('<b>x</b>');
  });

  it('draws nothing for a clip that was never aligned', async () => {
    for (const uncertainWords of [undefined, null, []] as const) {
      const { meta, hint } = await renderClip({ ...BASE, uncertainWords: uncertainWords as Clip['uncertainWords'] });
      expect(hint, `absent evidence (${JSON.stringify(uncertainWords)}) must not be drawn`).toBeNull();
      expect(meta).toBe('1.0s · Lamo');
    }
  });
});
