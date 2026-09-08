import { readFileSync } from 'node:fs';
import path from 'node:path';
// @ts-expect-error jsdom is Vitest's runtime dependency and has no bundled declarations.
import { JSDOM } from 'jsdom';
import { afterEach, describe, expect, it } from 'vitest';

const PAGE = path.resolve(__dirname, '..', 'src-tauri', 'assets', 'couch.html');
let active: JSDOM | null = null;

async function boot() {
  const dom = new JSDOM(readFileSync(PAGE, 'utf8'), {
    runScripts: 'dangerously',
    url: 'http://couch.test/?lang=en',
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    beforeParse(win: any) {
      win.fetch = async () => {
        throw new Error('no server in UI unit test');
      };
      win.HTMLMediaElement.prototype.pause = () => {};
      win.HTMLMediaElement.prototype.play = async () => {};
      win.HTMLMediaElement.prototype.load = () => {};
      win.HTMLCanvasElement.prototype.getContext = () => ({ clearRect() {}, fillRect() {} });
    },
  });
  active = dom;
  await dom.window.eval('load()');
  dom.window.eval(`
    me = 'Fixture reviewer';
    sessionStorage.setItem('cortex.couch.who', me);
    queue = [{id:'first', text:'دەقی یەکەم', rowVersion:'1', durationMs:4000, redo:true},
             {id:'second', text:'دەقی دووەم', rowVersion:'2', durationMs:4000}];
    i = 0; exhausted = true;
    preparePlayback = async () => null;
    prefetchNext = () => {};
    show();
    window.posts = [];
    finalizePlaybackForDecision = async () => 'fixture-receipt';
    api = async (url, options) => {
      if (url === '/api/decision') window.posts.push(JSON.parse(options.body));
      return {ok:true};
    };
  `);
  return dom;
}

function element(dom: JSDOM, id: string) {
  return dom.window.document.getElementById(id);
}

function type(dom: JSDOM, value: string) {
  element(dom, 'text').value = value;
  element(dom, 'text').dispatchEvent(new dom.window.Event('input'));
}

afterEach(() => {
  active?.window.close();
  active = null;
});

describe('guided review — existing server contracts, clearer client states', () => {
  it('never paints a late waveform over the newer clip and closes both decoders', async () => {
    const dom = await boot();
    dom.window.eval(`
      window.waveDecodes = []; window.closedDecoders = 0;
      fetch = async () => ({ok:true, arrayBuffer:async () => new ArrayBuffer(4)});
      window.AudioContext = class {
        decodeAudioData() { return new Promise(resolve => waveDecodes.push(resolve)); }
        async close() { closedDecoders++; }
      };
      window.oldWave = loadWave('/old-wave');
    `);
    await new Promise((resolve) => setTimeout(resolve, 0));
    dom.window.eval("window.newWave = loadWave('/new-wave')");
    await new Promise((resolve) => setTimeout(resolve, 0));
    dom.window.eval(
      `waveDecodes[1]({getChannelData: () => new Float32Array([1, ...Array(159).fill(0)])})`,
    );
    await dom.window.newWave;
    const current = Array.from(dom.window.eval('peaks'));
    dom.window.eval(`waveDecodes[0]({getChannelData: () => new Float32Array(160).fill(1)})`);
    await dom.window.oldWave;
    expect(Array.from(dom.window.eval('peaks'))).toEqual(current);
    expect(dom.window.closedDecoders).toBe(2);
  });

  it('closes a waveform decoder even when the media is corrupt', async () => {
    const dom = await boot();
    dom.window.eval(`
      window.closedDecoders = 0;
      fetch = async () => ({ok:true, arrayBuffer:async () => new ArrayBuffer(4)});
      window.AudioContext = class {
        async decodeAudioData() { throw new Error('corrupt'); }
        async close() { closedDecoders++; }
      };
    `);
    await dom.window.eval("loadWave('/broken-wave')");
    expect(dom.window.closedDecoders).toBe(1);
    expect(dom.window.eval('peaks')).toBeNull();
  });

  it('clears the prior waveform immediately when the visible revision changes', async () => {
    const dom = await boot();
    dom.window.eval('peaks = [1, 0, 1]; i = 1; show()');
    expect(dom.window.eval('peaks')).toBeNull();
  });

  it('removes the prior reviewer coin total during identity handover even if the next response omits accounting', async () => {
    const dom = await boot();
    dom.window.eval(`
      applyAccounting({accountingAvailable:true, reviewedMs:0, correctedMs:0,
        earnedMicroIqd:'1250000000', settledMicroIqd:'0', outstandingMicroIqd:'1250000000',
        legacyEventsPendingReconciliation:0});
      renderProgress();
    `);
    expect(element(dom, 'accounting').textContent).toBe('1,250');
    dom.window.eval(
      `adoptReviewerIdentity('Next reviewer'); applyAccounting({}); renderProgress();`,
    );
    expect(element(dom, 'accounting').hidden).toBe(true);
    expect(element(dom, 'accounting').textContent).toBe('');
  });
  it('keeps an unsaved in-memory correction through a same-revision refresh when draft storage fails', async () => {
    const dom = await boot();
    dom.window.eval(`
      const previousSet = Storage.prototype.setItem;
      Storage.prototype.setItem = function(key, value) {
        if (key.startsWith('cortex.couch.draft')) throw new Error('quota');
        return previousSet.call(this, key, value);
      };
    `);
    type(dom, 'a correction not yet stored');
    dom.window.eval('show()');
    expect(element(dom, 'text').value).toBe('a correction not yet stored');
    expect(element(dom, 'draftState').textContent).toContain('Failed to save');
    expect(dom.window.posts).toHaveLength(0);
  });

  it('does not reset composition text or edit-pause rewind on a same-revision refresh', async () => {
    const dom = await boot();
    element(dom, 'text').dispatchEvent(new dom.window.Event('compositionstart'));
    element(dom, 'text').value = 'composition still in progress';
    element(dom, 'text').setSelectionRange(2, 5);
    dom.window.eval('pausedByEdit = true; show()');
    expect(element(dom, 'text').value).toBe('composition still in progress');
    expect(element(dom, 'text').selectionStart).toBe(2);
    expect(element(dom, 'text').selectionEnd).toBe(5);
    expect(dom.window.eval('pausedByEdit')).toBe(true);
    expect(element(dom, 'save').disabled).toBe(true);
  });

  it.each(['refused', 'saved'])(
    'publishes fresh eligibility after an identity-held operation is %s on initial load',
    async (outcome) => {
      const dom = await boot();
      type(dom, 'old queued correction');
      dom.window.eval(`
        writeOperationRecord({ operationId: '70000000-0000-4000-8000-000000000001',
          id: 'first', action: 'edit', text: 'old queued correction', reviewer: me,
          rowVersion: '1', playbackReceiptId: 'old-finalized-receipt' }, Date.now(), 0);
        me = ''; queue = []; i = 0;
        window.queueReads = 0;
        api = async (path, options) => {
          if (path === '/api/decision') {
            window.posts.push(JSON.parse(options.body));
            if (${JSON.stringify(outcome)} === 'refused') throw Object.assign(new Error('stale round'), {status:409});
            return {ok:true};
          }
          if (path === '/api/queue') {
            window.queueReads++;
            const first = {id:'first', text:'fresh round draft', rowVersion:'3', redo:true, durationMs:4000};
            const second = {id:'second', text:'next fresh clip', rowVersion:'2', durationMs:4000};
            return {playbackContractVersion:4, reviewer:'Fixture reviewer',
              items: window.queueReads > 1 && ${JSON.stringify(outcome)} === 'saved' ? [second] : [first,second]};
          }
          throw new Error('unexpected endpoint');
        };
      `);
      await dom.window.eval('load()');
      expect(dom.window.queueReads).toBe(2);
      expect(dom.window.posts).toHaveLength(1);
      expect(dom.window.eval('readOutbox()')).toEqual([]);
      expect(element(dom, 'text').value).toBe(
        outcome === 'refused' ? 'fresh round draft' : 'next fresh clip',
      );
      if (outcome === 'refused')
        expect(element(dom, 'draftRecoveryText').value).toBe('old queued correction');
      else expect(element(dom, 'draftRecovery').hidden).toBe(true);
    },
  );

  it('keeps a same-round draft but archives it instead of filling a new review round', async () => {
    const dom = await boot();
    type(dom, 'ڕاستکردنەوەی کۆن');
    dom.window.eval('show()');
    expect(element(dom, 'text').value).toBe('ڕاستکردنەوەی کۆن');
    dom.window.eval("queue[0].rowVersion = '3'; queue[0].text = 'دەقی نوێ'; show()");
    expect(element(dom, 'text').value).toBe('دەقی نوێ');
    expect(element(dom, 'draftRecovery').hidden).toBe(false);
    expect(element(dom, 'draftRecoveryText').value).toBe('ڕاستکردنەوەی کۆن');
    expect(element(dom, 'draftRecoveryText').readOnly).toBe(true);
    expect(dom.window.posts).toHaveLength(0);
    type(dom, 'ڕاستکردنەوەی نوێ');
    dom.window.eval('show()');
    expect(element(dom, 'text').value).toBe('ڕاستکردنەوەی نوێ');
    expect(element(dom, 'draftRecoveryList').options).toHaveLength(2);
  });

  it('does not bind an unversioned legacy draft to a returned review round', async () => {
    const dom = await boot();
    dom.window.sessionStorage.setItem('cortex.couch.draft.first', 'legacy correction');
    dom.window.eval('show()');
    expect(element(dom, 'text').value).toBe('دەقی یەکەم');
    expect(element(dom, 'draftRecoveryText').value).toBe('legacy correction');
  });

  it('keeps the original draft when archival storage fails and shows fresh server text', async () => {
    const dom = await boot();
    type(dom, 'old correction');
    dom.window.eval(`
      const originalSet = Storage.prototype.setItem;
      Storage.prototype.setItem = function(key, value) {
        if (key.startsWith('cortex.couch.draft-recovery.')) throw new Error('quota');
        return originalSet.call(this, key, value);
      };
      queue[0].rowVersion = '3'; show();
    `);
    expect(element(dom, 'text').value).toBe('دەقی یەکەم');
    expect(dom.window.sessionStorage.getItem('cortex.couch.draft.first')).toBe('old correction');
    expect(element(dom, 'draftRecoveryText').value).toBe('old correction');
    expect(element(dom, 'draftState').textContent).toContain('Failed to save');
  });

  it('hides recovery text until this tab and the server agree on the reviewer', async () => {
    const dom = await boot();
    type(dom, 'private draft');
    expect(element(dom, 'draftRecoveryText').value).toBe('private draft');
    dom.window.eval("me = ''; renderDraftRecovery()");
    expect(element(dom, 'draftRecovery').hidden).toBe(true);
    expect(element(dom, 'draftRecoveryText').value).toBe('');
    dom.window.eval("me = 'Another reviewer'; renderDraftRecovery()");
    expect(element(dom, 'draftRecoveryText').value).toBe('');
    expect(dom.window.posts).toHaveLength(0);
  });

  it('uses the exact installed, licensed icon definitions rather than custom drawings', async () => {
    const dom = await boot();
    const icons = JSON.parse(element(dom, 'couch-icons').textContent);
    for (const [name, nodes] of Object.entries(icons)) {
      const source = readFileSync(
        path.resolve(
          __dirname,
          '..',
          'node_modules',
          '@lucide',
          'svelte',
          'dist',
          'icons',
          `${name}.svelte`,
        ),
        'utf8',
      );
      const match = source.match(/const iconNode = (\[[^\n]+\]);/);
      expect(match, name).not.toBeNull();
      expect(nodes).toEqual(JSON.parse(match![1]));
    }
  });

  it('retains corrections on the legacy accept handler without showing two primary buttons', async () => {
    const dom = await boot();
    type(dom, 'ڕاستکراوە');
    expect(element(dom, 'accept').hidden).toBe(true);
    await dom.window.eval('decide("accept")');
    expect(dom.window.posts[0]).toMatchObject({ action: 'accept', text: 'ڕاستکراوە', id: 'first' });
  });

  it('keeps an unacknowledged submission visibly saving and cannot double-submit', async () => {
    const dom = await boot();
    type(dom, 'ڕاستکراوە');
    dom.window.eval(
      `api = async (url, options) => { window.posts.push(JSON.parse(options.body)); return new Promise(resolve => { window.ack = resolve; }); }; window.submitting = decide('edit');`,
    );
    await Promise.resolve();
    await Promise.resolve();
    expect(element(dom, 'submitHint').textContent).toBe('Saving...');
    expect(element(dom, 'save').disabled).toBe(true);
    expect(element(dom, 'after').hidden).toBe(true);
    await dom.window.eval('decide("edit")');
    expect(dom.window.posts).toHaveLength(1);
    dom.window.ack({ ok: true });
    await dom.window.submitting;
    expect(element(dom, 'afterLabel').textContent).toBe('Saved');
    expect(dom.window.eval('queue[i].id')).toBe('second');
  });

  it('reports a draft-storage failure while retaining the visible correction', async () => {
    const dom = await boot();
    dom.window
      .eval(`const originalSet = Storage.prototype.setItem; Storage.prototype.setItem = function(key, value) {
      if (key.startsWith('cortex.couch.draft.')) throw new DOMException('full', 'QuotaExceededError');
      return originalSet.call(this,key,value);
    };`);
    type(dom, 'ڕاستکراوە');
    expect(element(dom, 'draftState').textContent).toContain('Failed to save');
    expect(element(dom, 'text').value).toBe('ڕاستکراوە');
    expect(element(dom, 'save').hidden).toBe(false);
    expect(dom.window.posts).toHaveLength(0);
  });

  it('has one primary action, switching to correction and back without replacing media', async () => {
    const dom = await boot();
    const player = element(dom, 'player');
    const text = element(dom, 'text');
    expect(element(dom, 'accept').hidden).toBe(false);
    expect(element(dom, 'save').hidden).toBe(true);
    type(dom, 'ڕاستکراوە');
    expect(element(dom, 'accept').hidden).toBe(true);
    expect(element(dom, 'save').hidden).toBe(false);
    expect(element(dom, 'draftState').textContent).toBe('Edited · Not submitted');
    type(dom, 'دەقی یەکەم');
    expect(element(dom, 'accept').hidden).toBe(false);
    expect(element(dom, 'save').hidden).toBe(true);
    expect(element(dom, 'player')).toBe(player);
    expect(element(dom, 'text')).toBe(text);
  });

  it.each([
    ['é', 'e\u0301', true],
    ['hello world', '  HELLO\u0085WORLD\u00a0', true],
    ['hello world', 'hello world!', false],
    ['دەق', '\ufeffدەق', false],
    ['ک', 'ك', false],
    ['ی', 'ي', false],
    ['دەق', 'دە\u200cق', false],
  ])('presentation keys compare %j and %j without Sorani folding', async (a, b, equal) => {
    const dom = await boot();
    expect(
      dom.window.eval(
        `presentationTextKey(${JSON.stringify(a)}) === presentationTextKey(${JSON.stringify(b)})`,
      ),
    ).toBe(equal);
  });

  it('does not submit, skip or undo while an IME composition is in progress', async () => {
    const dom = await boot();
    element(dom, 'text').dispatchEvent(new dom.window.Event('compositionstart'));
    type(dom, 'دەقی نوێ');
    for (const id of ['save', 'accept', 'skip', 'bad', 'undo'])
      expect(element(dom, id).disabled).toBe(true);
    await dom.window.eval('decide("edit")');
    await dom.window.eval('decide("skip")');
    expect(dom.window.posts).toHaveLength(0);
    element(dom, 'text').dispatchEvent(new dom.window.Event('compositionend'));
    expect(element(dom, 'save').disabled).toBe(false);
    await dom.window.eval('decide("edit")');
    expect(dom.window.posts).toHaveLength(1);
    expect(dom.window.posts[0]).toMatchObject({ id: 'first', text: 'دەقی نوێ', action: 'edit' });
  });

  it('missing audio blocks all judgments but leaves a no-verdict skip', async () => {
    const dom = await boot();
    dom.window.eval('showPlaybackWarning("audioMissing")');
    for (const id of ['save', 'accept', 'bad']) expect(element(dom, id).disabled).toBe(true);
    expect(element(dom, 'skip').disabled).toBe(false);
    expect(element(dom, 'retryAudio').hidden).toBe(false);
    await dom.window.eval('decide("accept")');
    expect(dom.window.posts).toHaveLength(0);
    await dom.window.eval('decide("skip")');
    expect(dom.window.posts).toHaveLength(1);
    expect(dom.window.posts[0].action).toBe('skip');
  });

  it('an empty edit cannot be confirmed but can be skipped', async () => {
    const dom = await boot();
    type(dom, '  ');
    expect(element(dom, 'save').disabled).toBe(true);
    expect(element(dom, 'accept').hidden).toBe(true);
    expect(element(dom, 'skip').disabled).toBe(false);
    expect(element(dom, 'submitHint').textContent).toContain('Enter a transcript');
  });

  it('a failed outbox write never posts, advances, or claims a saved correction', async () => {
    const dom = await boot();
    type(dom, 'ڕاستکراوە');
    dom.window.eval(`
      const originalSetItem = Storage.prototype.setItem;
      Storage.prototype.setItem = function(key, value) {
        if (key.startsWith('cortex.couch.outbox.operation.')) throw new DOMException('full', 'QuotaExceededError');
        return originalSetItem.call(this, key, value);
      };
    `);
    await dom.window.eval('decide("edit")');
    expect(dom.window.posts).toHaveLength(0);
    expect(dom.window.eval('queue[i].id')).toBe('first');
    expect(element(dom, 'text').value).toBe('ڕاستکراوە');
    expect(element(dom, 'toast').textContent).toContain('Failed to save');
    expect(element(dom, 'save').disabled).toBe(false);
  });

  it('language and text-size changes preserve text, selection, player and current time', async () => {
    const dom = await boot();
    type(dom, 'ڕاستکراوە');
    element(dom, 'text').setSelectionRange(2, 4);
    const player = element(dom, 'player');
    player.currentTime = 2;
    element(dom, 'lang').click();
    element(dom, 'textsize').click();
    expect(element(dom, 'text').value).toBe('ڕاستکراوە');
    expect(element(dom, 'text').selectionStart).toBe(2);
    expect(element(dom, 'text').selectionEnd).toBe(4);
    expect(element(dom, 'player')).toBe(player);
    expect(player.currentTime).toBe(2);
    expect(element(dom, 'save').hidden).toBe(false);
    expect(dom.window.document.documentElement.dir).toBe('rtl');
  });

  it('transport labels expose state and the seek control resets coverage baseline', async () => {
    const dom = await boot();
    element(dom, 'loop').click();
    expect(element(dom, 'loop').textContent).toBe('Loop On');
    expect(element(dom, 'loop').getAttribute('aria-pressed')).toBe('true');
    expect(element(dom, 'play').getAttribute('aria-label')).toBe('Play');
    Object.defineProperty(element(dom, 'player'), 'duration', { value: 4, configurable: true });
    dom.window.eval(
      'renderTransportLabels(); window.resets = 0; resetPlaybackTraversalBaseline = () => { window.resets++; };',
    );
    element(dom, 'seek').value = '2';
    element(dom, 'seek').dispatchEvent(new dom.window.Event('input'));
    expect(dom.window.resets).toBe(1);
    expect(element(dom, 'player').currentTime).toBe(2);
    expect(element(dom, 'seek').getAttribute('aria-valuetext')).toBe('0:02 / 0:04');
  });
});
