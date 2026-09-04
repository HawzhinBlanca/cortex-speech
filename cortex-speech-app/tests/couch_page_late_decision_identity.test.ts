import { readFileSync } from 'fs';
import path from 'path';
// @ts-expect-error jsdom is Vitest's runtime dependency and has no bundled declarations.
import { JSDOM } from 'jsdom';
import { afterEach, describe, expect, it } from 'vitest';

const PAGE = path.resolve(__dirname, '..', 'src-tauri', 'assets', 'couch.html');
let active: JSDOM | null = null;

async function bootPage() {
  const dom = new JSDOM(readFileSync(PAGE, 'utf-8'), {
    runScripts: 'dangerously',
    url: 'http://couch.test/',
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    beforeParse(win: any) {
      win.fetch = () => Promise.reject(new Error('no network in identity test'));
      win.HTMLMediaElement.prototype.pause = () => {};
      win.HTMLMediaElement.prototype.load = () => {};
      win.HTMLMediaElement.prototype.play = async () => {};
      win.HTMLCanvasElement.prototype.getContext = () => ({ clearRect() {}, fillRect() {} });
    },
  });
  active = dom;
  await dom.window.eval('load()');
  dom.window.eval(`
    window.nextReviewer = 'Sara';
    window.items = [
      { id: 'shared', text: 'server draft', durationMs: 1500, rowVersion: '1' },
      { id: 'next', text: 'next server draft', durationMs: 1500, rowVersion: '2' }
    ];
    api = async (path) => {
      if (path === '/api/queue') return {
        playbackContractVersion: 4, reviewer: window.nextReviewer,
        items: window.items, heldByOthers: 0
      };
      throw new Error('no playback service in storage test');
    };
  `);
  await dom.window.eval('load()');
  return dom;
}

function typeDraft(dom: JSDOM, value: string) {
  const input = dom.window.document.getElementById('text') as HTMLTextAreaElement;
  input.value = value;
  input.dispatchEvent(new dom.window.Event('input'));
}

async function startPendingDecision(dom: JSDOM, route: 'outbox' | 'online') {
  typeDraft(dom, 'Sara submitted correction');
  dom.window.eval(`
    window.decisionStarted = new Promise(resolve => { window.signalStarted = resolve; });
    window.decisionReply = new Promise((resolve, reject) => {
      window.resolveDecision = resolve; window.rejectDecision = reject;
    });
    window.decisionCalls = 0;
    const previousApi = api;
    api = async (path, options) => {
      if (path !== '/api/decision') return previousApi(path, options);
      window.decisionCalls += 1;
      if (window.decisionCalls === 1) {
        window.signalStarted(); return window.decisionReply;
      }
      // The shared cookie changed while the first request's reply was delayed.
      throw Object.assign(new Error('decision was made by Sara, not Hemn'), { status: 409 });
    };
    finalizePlaybackForDecision = async () => 'test-finalized-receipt';
  `);
  if (route === 'outbox') {
    dom.window.eval(`queueSubmission({
      operationId: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', id: 'shared',
      action: 'edit', text: 'Sara submitted correction', reviewer: 'Sara', rowVersion: '1',
      playbackReceiptId: 'test-finalized-receipt'
    }); window.pendingDecision = flushOutbox();`);
  } else {
    dom.window.eval('window.pendingDecision = decide("edit")');
  }
  await dom.window.eval('window.decisionStarted');
}

afterEach(() => {
  active?.window.close();
  active = null;
});

describe('couch.html — delayed decisions across a shared-phone identity change', () => {
  it('a new reviewer obtains a fresh playback grant even for the same clip revision', async () => {
    const dom = await bootPage();
    dom.window.eval(`
      playbackAttempt = { identity: playbackIdentity(queue[i]), playbackReceiptId: 'Sara-old-grant' };
      playbackTraversalIdentity = playbackIdentity(queue[i]);
      $('player').src = proofAudioUrl(queue[i], 'Sara-old-grant');
      window.playbackStarts = 0;
      const previousApi = api;
      api = async (path, options) => {
        if (path === '/api/playback/start') {
          window.playbackStarts += 1;
          return new Promise(() => {}); // fresh grant deliberately pending; no physical playback
        }
        return previousApi(path, options);
      };
      window.nextReviewer = 'Hemn';
    `);
    await dom.window.eval('load()');
    expect(dom.window.eval('window.playbackStarts')).toBe(1);
    expect(dom.window.document.getElementById('player')!.getAttribute('src')).toBeNull();
  });

  it('a new reviewer does not inherit the previous reviewer session progress', async () => {
    const dom = await bootPage();
    await startPendingDecision(dom, 'online');
    dom.window.eval('window.resolveDecision({})');
    await dom.window.eval('window.pendingDecision');
    expect(dom.window.eval('doneThisSession')).toBe(1);
    dom.window.eval("window.nextReviewer = 'Hemn'");
    await dom.window.eval('load()');
    expect(dom.window.eval('doneThisSession')).toBe(0);
    expect(dom.window.document.getElementById('progress')!.textContent).not.toContain('✓1');
  });

  it('a previous reviewer success cannot suppress the new reviewer refused-work warning', async () => {
    const dom = await bootPage();
    await startPendingDecision(dom, 'online');
    dom.window.eval('window.resolveDecision({})');
    await dom.window.eval('window.pendingDecision');
    dom.window.eval("window.nextReviewer = 'Hemn'; window.items = [window.items[0]]");
    await dom.window.eval('load()');
    dom.window.eval(`
      queueSubmission({ operationId: 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb', id: 'shared',
        action: 'edit', text: 'Hemn correction', reviewer: 'Hemn', rowVersion: '1',
        playbackReceiptId: 'Hemn-finalized-receipt' });
      api = async () => { throw Object.assign(new Error('invalid submission'), { status: 400 }); };
    `);
    await dom.window.eval('flushOutbox()');
    expect(JSON.parse(dom.window.localStorage.getItem('cortex.couch.refused') || '[]')).toEqual([
      { id: 'shared', by: 'Hemn' },
    ]);
  });

  it('a failed finalization for the previous reviewer leaves the new reviewer screen untouched', async () => {
    const dom = await bootPage();
    dom.window.eval(`
      window.finalizeStarted = new Promise(resolve => { window.signalFinalize = resolve; });
      finalizePlaybackForDecision = async () => {
        window.signalFinalize();
        return new Promise((_resolve, reject) => { window.rejectFinalize = reject; });
      };
      window.pendingDecision = decide('edit');
    `);
    await dom.window.eval('window.finalizeStarted');
    dom.window.eval("window.nextReviewer = 'Hemn'");
    await dom.window.eval('load()');
    const before = dom.window.document.getElementById('toast')!.textContent;
    dom.window.eval(
      "window.rejectFinalize(Object.assign(new Error('old attempt unavailable'), { status: 428 }))",
    );
    await dom.window.eval('window.pendingDecision');
    expect(dom.window.document.getElementById('toast')!.textContent).toBe(before);
    expect(dom.window.eval('readOutbox()')).toEqual([]);
    expect(dom.window.eval('busy')).toBe(false);
  });

  it.each(['identity', 'revision'] as const)(
    'finalization cannot submit after the displayed %s changes',
    async (change) => {
      const dom = await bootPage();
      typeDraft(dom, 'Sara clicked correction');
      dom.window.eval(`
      window.finalizeStarted = new Promise(resolve => { window.signalFinalize = resolve; });
      finalizePlaybackForDecision = async () => {
        window.signalFinalize();
        return new Promise(resolve => { window.resolveFinalize = resolve; });
      };
      window.posted = [];
      const previousApi = api;
      api = async (path, options) => {
        if (path === '/api/decision') { window.posted.push(JSON.parse(options.body)); return {}; }
        return previousApi(path, options);
      };
      window.pendingDecision = decide('edit');
    `);
      await dom.window.eval('window.finalizeStarted');
      if (change === 'identity') dom.window.eval("window.nextReviewer = 'Hemn'");
      else
        dom.window.eval(
          "window.items = window.items.map(item => ({ ...item, rowVersion: 'new' }))",
        );
      await dom.window.eval('load()');
      typeDraft(dom, 'new displayed correction');
      dom.window.eval("window.resolveFinalize('old-finalized-receipt')");
      await dom.window.eval('window.pendingDecision');
      expect(dom.window.eval('window.posted')).toEqual([]);
      expect(dom.window.eval('readOutbox()')).toEqual([]);
      expect(dom.window.eval('i')).toBe(0);
      expect(dom.window.eval('busy')).toBe(false);
      expect((dom.window.document.getElementById('text') as HTMLTextAreaElement).value).toBe(
        'new displayed correction',
      );
    },
  );

  it.each(['outbox', 'online'] as const)(
    '%s acknowledgement preserves a newer draft by the same reviewer',
    async (route) => {
      const dom = await bootPage();
      await startPendingDecision(dom, route);
      typeDraft(dom, 'Sara newer unsent correction');
      dom.window.eval('window.resolveDecision({})');
      await dom.window.eval('window.pendingDecision');
      expect(dom.window.sessionStorage.getItem('cortex.couch.draft.shared')).toBe(
        'Sara newer unsent correction',
      );
      expect(dom.window.eval('readOutbox().length')).toBe(0);
    },
  );

  it('normal queue loading clears the previous reviewer draft before showing the next reviewer', async () => {
    const dom = await bootPage();
    typeDraft(dom, 'Sara private draft');
    dom.window.eval("window.nextReviewer = 'Hemn'");
    await dom.window.eval('load()');
    expect(dom.window.eval('me')).toBe('Hemn');
    expect((dom.window.document.getElementById('text') as HTMLTextAreaElement).value).toBe(
      'server draft',
    );
    expect(dom.window.sessionStorage.getItem('cortex.couch.draft.shared')).toBeNull();
  });

  it.each(['outbox', 'online'] as const)(
    '%s acknowledgement cannot erase the next reviewer draft',
    async (route) => {
      const dom = await bootPage();
      await startPendingDecision(dom, route);
      dom.window.eval("window.nextReviewer = 'Hemn'");
      await dom.window.eval('load()');
      typeDraft(dom, 'Hemn independent draft');
      dom.window.eval('window.resolveDecision({})');
      await dom.window.eval('window.pendingDecision');
      expect(dom.window.sessionStorage.getItem('cortex.couch.draft.shared')).toBe(
        'Hemn independent draft',
      );
      expect(dom.window.eval('i')).toBe(0);
      expect((dom.window.document.getElementById('text') as HTMLTextAreaElement).value).toBe(
        'Hemn independent draft',
      );
      expect(dom.window.eval('doneThisSession')).toBe(0);
      expect(dom.window.eval('readOutbox().length')).toBe(0);
    },
  );

  it.each([409, 428, 503])(
    'online refusal %s cannot navigate or rearm the next reviewer clip',
    async (status) => {
      const dom = await bootPage();
      await startPendingDecision(dom, 'online');
      dom.window.eval("window.nextReviewer = 'Hemn'");
      await dom.window.eval('load()');
      typeDraft(dom, 'Hemn independent draft');
      dom.window
        .eval(`window.rearmCalls = 0; rearmPlayback = async () => { window.rearmCalls += 1; };
      window.rejectDecision(Object.assign(new Error('response belongs to Sara'), { status: ${status} }));`);
      await dom.window.eval('window.pendingDecision');
      expect(dom.window.eval('i')).toBe(0);
      expect(dom.window.eval('window.rearmCalls')).toBe(0);
      expect((dom.window.document.getElementById('text') as HTMLTextAreaElement).value).toBe(
        'Hemn independent draft',
      );
      expect(dom.window.eval('readOutbox().map(item => item.reviewer)')).toEqual(['Sara']);
      expect(dom.window.eval('busy')).toBe(false);
    },
  );
});
