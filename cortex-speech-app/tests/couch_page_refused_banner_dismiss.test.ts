import { readFileSync } from 'fs';
import path from 'path';
// @ts-expect-error jsdom is already Vitest's runtime dependency but ships no bundled declarations.
import { JSDOM } from 'jsdom';
import { afterEach, describe, expect, it } from 'vitest';

/**
 * The refused-decisions banner is the phone's local memory of saves the server refused. A tap jumps
 * to a refused clip if one is in the batch on screen. When none is — the usual case, because someone
 * else took the clip, or the entry predates this reviewer on a shared phone — nothing can be done
 * about it, and the tap used to do nothing, so the banner stayed for the life of the browser profile.
 * Measured 2026-09-04 on the owner's phone: "187 decisions could not be saved", none actionable.
 *
 * Pinned here: a tap with nothing to jump to dismisses the entries this reviewer can see (their own
 * and the unstamped legacy ones) and leaves other reviewers' stamped entries alone; a tap with a
 * refused clip in the batch still jumps and keeps the list.
 */
const PAGE = path.resolve(__dirname, '..', 'src-tauri', 'assets', 'couch.html');
const REFUSED = 'cortex.couch.refused';

let active: JSDOM | null = null;

async function bootPage(): Promise<JSDOM> {
  const dom = new JSDOM(readFileSync(PAGE, 'utf-8'), {
    runScripts: 'dangerously',
    url: 'http://127.0.0.1:8737/',
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    beforeParse(win: any) {
      win.fetch = () => Promise.reject(new Error('offline refused-banner test'));
      // This suite verifies storage/ownership, not physical playback in jsdom.
      win.HTMLMediaElement.prototype.pause = () => {};
      win.HTMLMediaElement.prototype.load = () => {};
      win.HTMLCanvasElement.prototype.getContext = () => ({
        clearRect: () => {},
        fillRect: () => {},
        fillStyle: '',
      });
    },
  });
  await dom.window.eval('load()');
  dom.window.eval(
    "me = 'Sara'; exhausted = true; queue = [" +
      "{ id: 'c1', text: 'یەکەم', durationMs: 1000, rowVersion: '1' }," +
      "{ id: 'c2', text: 'دووەم', durationMs: 1000, rowVersion: '2' }]; i = 0; show(false);",
  );
  active = dom;
  return dom;
}

afterEach(() => {
  active?.window.close();
  active = null;
});

describe('couch.html — the refused-decisions banner', () => {
  it('records separate refusals for two reviewers of the same clip', async () => {
    const dom = await bootPage();
    dom.window.eval(
      "noteRefused('shared'); me = 'Hemn'; noteRefused('shared'); noteRefused('shared');",
    );
    expect(JSON.parse(dom.window.localStorage.getItem(REFUSED) || '[]')).toEqual([
      { id: 'shared', by: 'Sara' },
      { id: 'shared', by: 'Hemn' },
    ]);
  });

  it('a successful save clears only that reviewer and explicit legacy records for the clip', async () => {
    const dom = await bootPage();
    dom.window.localStorage.setItem(
      REFUSED,
      JSON.stringify([
        { id: 'shared', by: 'Sara' },
        { id: 'shared', by: 'Hemn' },
        'shared',
        { id: 'another', by: 'Sara' },
      ]),
    );
    dom.window.eval("clearRefused('shared')");
    expect(JSON.parse(dom.window.localStorage.getItem(REFUSED) || '[]')).toEqual([
      { id: 'shared', by: 'Hemn' },
      { id: 'another', by: 'Sara' },
    ]);
  });

  it("dismissal keeps a colleague's refusal even for the identical clip ID", async () => {
    const dom = await bootPage();
    dom.window.localStorage.setItem(
      REFUSED,
      JSON.stringify([{ id: 'gone', by: 'Sara' }, { id: 'gone', by: 'Hemn' }, 'legacy']),
    );
    const pendingKey = dom.window.eval(
      "outboxOperationKey('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa')",
    );
    dom.window.localStorage.setItem(pendingKey, 'pending-work-must-remain');
    dom.window.sessionStorage.setItem(dom.window.eval("draftKey('gone')"), 'typed correction');
    dom.window.eval('renderRefused()');
    dom.window.document.getElementById('err')!.click();
    expect(JSON.parse(dom.window.localStorage.getItem(REFUSED) || '[]')).toEqual([
      { id: 'gone', by: 'Hemn' },
    ]);
    expect(dom.window.localStorage.getItem(pendingKey)).toBe('pending-work-must-remain');
    expect(dom.window.sessionStorage.getItem(dom.window.eval("draftKey('gone')"))).toBe(
      'typed correction',
    );
    dom.window.eval("me = 'Hemn'; renderRefused()");
    expect((dom.window.document.getElementById('err') as HTMLElement).hidden).toBe(false);
  });

  it("unknown identity cannot clear another stamped reviewer's warning", async () => {
    const dom = await bootPage();
    dom.window.localStorage.setItem(REFUSED, JSON.stringify([{ id: 'gone', by: 'Hemn' }]));
    dom.window.eval("me = ''; renderRefused(); clearRefused('gone')");
    dom.window.document.getElementById('err')!.click();
    expect(JSON.parse(dom.window.localStorage.getItem(REFUSED) || '[]')).toEqual([
      { id: 'gone', by: 'Hemn' },
    ]);
    expect((dom.window.document.getElementById('err') as HTMLElement).hidden).toBe(false);
  });

  it('late replies keep the submitting reviewer identity after the displayed identity changes', async () => {
    const dom = await bootPage();
    dom.window.localStorage.setItem(
      REFUSED,
      JSON.stringify([
        { id: 'gone', by: 'Sara' },
        { id: 'gone', by: 'Hemn' },
      ]),
    );
    dom.window.eval("me = 'Hemn'; clearRefused('gone', 'Sara'); noteRefused('other', 'Sara')");
    expect(JSON.parse(dom.window.localStorage.getItem(REFUSED) || '[]')).toEqual([
      { id: 'gone', by: 'Hemn' },
      { id: 'other', by: 'Sara' },
    ]);
  });

  it('unknown identity can acknowledge legacy entries without dismissing named reviewers', async () => {
    const dom = await bootPage();
    dom.window.localStorage.setItem(
      REFUSED,
      JSON.stringify(['legacy', { id: 'gone', by: 'Hemn' }]),
    );
    dom.window.eval("me = ''; renderRefused()");
    dom.window.document.getElementById('err')!.click();
    expect(JSON.parse(dom.window.localStorage.getItem(REFUSED) || '[]')).toEqual([
      { id: 'gone', by: 'Hemn' },
    ]);
    expect((dom.window.document.getElementById('err') as HTMLElement).hidden).toBe(false);
  });

  it.each([false, true])(
    'outbox replies retain their author across an identity change (refused=%s)',
    async (refused) => {
      const dom = await bootPage();
      const before = [{ id: 'shared', by: 'Hemn' }];
      if (!refused) before.push({ id: 'shared', by: 'Sara' });
      dom.window.localStorage.setItem(REFUSED, JSON.stringify(before));
      dom.window.eval(
        "writeOperationRecord({ operationId: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', " +
          "id: 'shared', action: 'edit', text: 'ڕاستکراوە', reviewer: 'Sara', heardMs: 1500, clipDurationMs: 1500 }, Date.now(), 0)",
      );
      // The page's actual flush awaits a response after the displayed reviewer has changed.
      dom.window.eval(
        "api = async () => { me = 'Hemn'; " +
          (refused
            ? "throw Object.assign(new Error('invalid decision'), { status: 400 });"
            : 'return {};') +
          ' }',
      );
      await dom.window.eval('flushOutboxOnce()');
      expect(JSON.parse(dom.window.localStorage.getItem(REFUSED) || '[]')).toEqual(
        refused
          ? [
              { id: 'shared', by: 'Hemn' },
              { id: 'shared', by: 'Sara' },
            ]
          : [{ id: 'shared', by: 'Hemn' }],
      );
    },
  );

  it('a tap with a refused clip in the batch jumps to it and keeps the list', async () => {
    const dom = await bootPage();
    dom.window.localStorage.setItem(REFUSED, JSON.stringify([{ id: 'c2', by: 'Sara' }]));
    dom.window.eval('renderRefused()');
    const err = dom.window.document.getElementById('err') as HTMLElement;
    expect(err.hidden).toBe(false);
    err.click();
    expect(dom.window.eval('i')).toBe(1);
    expect(dom.window.localStorage.getItem(REFUSED)).not.toBeNull();
  });

  it("a tap with nothing to jump to dismisses my entries and the legacy ones, not a colleague's", async () => {
    const dom = await bootPage();
    dom.window.localStorage.setItem(
      REFUSED,
      JSON.stringify([
        { id: 'gone-1', by: 'Sara' },
        'legacy-plain-id',
        { id: 'theirs', by: 'Hemn' },
      ]),
    );
    dom.window.eval('renderRefused()');
    const err = dom.window.document.getElementById('err') as HTMLElement;
    expect(err.hidden, 'two entries concern Sara, so the banner is up').toBe(false);
    expect(err.textContent).toContain('2');

    err.click();
    expect(dom.window.eval('i'), 'no jump to a wrong clip').toBe(0);
    expect(err.hidden, 'the banner comes down').toBe(true);
    const left = JSON.parse(dom.window.localStorage.getItem(REFUSED) || '[]');
    expect(left, "a colleague's stamped refusal survives for them").toEqual([
      { id: 'theirs', by: 'Hemn' },
    ]);
  });
});
