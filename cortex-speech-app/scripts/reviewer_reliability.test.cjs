// Synthetic DOM-only tests: no credentials, real audio, or production HTTP requests.
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const assert = require('node:assert/strict');
const { test } = require('node:test');
const { JSDOM } = require('jsdom');
const html = readFileSync(join(__dirname, '../src-tauri/assets/couch.html'), 'utf8');

async function boot(storage = {}) {
  const dom = new JSDOM(html, {
    runScripts: 'dangerously', url: 'http://couch.test/?lang=en',
    beforeParse(win) {
      for (const [key, value] of Object.entries(storage)) win.localStorage.setItem(key, value);
      win.fetch = async () => { throw new Error('Synthetic test: network disabled'); };
      win.HTMLMediaElement.prototype.pause = () => {};
      win.HTMLMediaElement.prototype.play = async () => {};
      win.HTMLMediaElement.prototype.load = () => {};
      win.HTMLCanvasElement.prototype.getContext = () => ({ clearRect() {}, fillRect() {} });
    },
  });
  await dom.window.eval('load()');
  dom.window.eval(`
    me = 'Fixture reviewer'; sessionStorage.setItem('cortex.couch.who', me);
    queue = [{id:'first',text:'original first',rowVersion:'1',durationMs:4000},
             {id:'second',text:'original second',rowVersion:'2',durationMs:4000}];
    i=0; exhausted=true; preparePlayback=async()=>null; prefetchNext=()=>{};
    finalizePlaybackForDecision=async()=>'fixture-receipt';
    window.actualApi=api; window.submissions=[];
    api=async(url,opts)=> {
      if (url==='/api/decision') {
        const submission=JSON.parse(opts.body); submissions.push(submission);
        return submission.action==='skip' ? {ok:true} : {ok:true,poolDecisionId:'1'};
      }
      return {ok:true};
    };
    show();
  `);
  return dom;
}

test('acknowledged pool Undo survives reload and remains reviewer scoped', async () => {
  let dom = await boot();
  try {
    await dom.window.eval("decide('accept')");
    const stored = Object.fromEntries(Object.keys(dom.window.localStorage).map(k => [k, dom.window.localStorage.getItem(k)]));
    dom.window.close();
    dom = await boot(stored);
    assert.ok(dom.window.eval('readPoolUndoTarget()'));
    assert.equal(dom.window.document.getElementById('after').hidden, false);
    dom.window.eval("me='Different reviewer';renderProgress()");
    assert.equal(dom.window.document.getElementById('after').hidden, true);
  } finally { dom.window.close(); }
});

test('draft quota failure blocks Skip without losing text or sending a decision', async () => {
  const dom = await boot();
  try {
    dom.window.eval(`
      window.storageSet=Storage.prototype.setItem;
      Storage.prototype.setItem=function(k,v) {
        if(k.startsWith('cortex.couch.draft')) throw new Error('quota');
        return storageSet.call(this,k,v);
      };
      $('text').value='precious unsaved correction';
      $('text').dispatchEvent(new Event('input'));
    `);
    assert.equal(dom.window.document.getElementById('skip').disabled, true);
    assert.equal(dom.window.document.getElementById('undo').disabled, true);
    await dom.window.eval("decide('skip')");
    assert.equal(dom.window.eval('i'), 0);
    assert.equal(dom.window.document.getElementById('text').value, 'precious unsaved correction');
    assert.equal(dom.window.submissions.length, 0);
    dom.window.eval('i=1;show();i=0;show()');
    assert.equal(dom.window.document.getElementById('text').value, 'precious unsaved correction');
    assert.equal(dom.window.document.getElementById('draftRecovery').hidden, false);
    dom.window.eval("Storage.prototype.setItem=storageSet;$('text').dispatchEvent(new Event('input'))");
    await dom.window.eval("decide('skip')");
    assert.equal(dom.window.eval('i'), 1);
    dom.window.eval('i=0;show()');
    assert.equal(dom.window.document.getElementById('text').value, 'precious unsaved correction');
  } finally { dom.window.close(); }
});

for (const success of [true, false]) {
  test(`timeout covers a stalled ${success ? 'JSON success' : 'error text'} response body`, async () => {
    const dom = await boot();
    try {
      dom.window.eval(`
        window.pendingTimers=new Map();window.timerNo=0;window.bodySettled=false;
        window.savedTimeout=setTimeout;window.savedClear=clearTimeout;
        setTimeout=(fn,ms)=>{const id=++timerNo;pendingTimers.set(id,{fn,ms});return id;};
        clearTimeout=id=>pendingTimers.delete(id);
        fetch=async(_url,options)=> {
          const read=()=>new Promise((_resolve,reject)=>options.signal.addEventListener('abort',()=>reject(new Error('aborted'))));
          return {ok:${success},status:503,headers:{get:()=> 'application/json'},json:read,text:read};
        };
        window.stalled=actualApi('/api/decision',{}).then(()=>{bodySettled=true;},()=>{bodySettled=true;});
      `);
      await new Promise(resolve => setTimeout(resolve, 0));
      const timeout = [...dom.window.pendingTimers.values()].find(timer => timer.ms === 15000);
      assert.ok(timeout, 'body must retain an active abort deadline');
      assert.equal(dom.window.bodySettled, false);
      timeout.fn();
      await dom.window.stalled;
      assert.equal(dom.window.bodySettled, true);
      assert.equal(dom.window.pendingTimers.size, 0);
    } finally {
      dom.window.eval('setTimeout=savedTimeout;clearTimeout=savedClear');
      dom.window.close();
    }
  });
}

test('renewal refusal stops cached playback and preserves the typed correction', async () => {
  const dom = await boot();
  try {
    Object.defineProperty(dom.window.document, 'hidden', { value: false, configurable: true });
    dom.window.eval(`
      $('player').src='/api/audio/first';
      $('text').value='retain this correction';$('text').dispatchEvent(new Event('input'));
      api=async()=>{const error=new Error('assignment changed');error.status=409;throw error;};
    `);
    await dom.window.eval('renewLease()');
    assert.equal(dom.window.document.getElementById('player').getAttribute('src'), null);
    assert.equal(dom.window.document.getElementById('save').disabled, true);
    assert.equal(dom.window.document.getElementById('text').value, 'retain this correction');
    assert.match(dom.window.document.getElementById('err').textContent, /assignment has changed/);
  } finally { dom.window.close(); }
});

test('late renewal refusal cannot disable a different clip', async () => {
  const dom = await boot();
  try {
    Object.defineProperty(dom.window.document, 'hidden', { value: false, configurable: true });
    dom.window.eval(`
      api=()=>new Promise((_resolve,reject)=>{window.rejectRenew=reject;});
      window.renewing=renewLease();i=1;show();
      $('player').src='/api/audio/second';
      const error=new Error('old assignment');error.status=409;rejectRenew(error);
    `);
    await dom.window.renewing;
    assert.equal(dom.window.document.getElementById('player').getAttribute('src'), '/api/audio/second');
    assert.equal(dom.window.eval('audioUnavailable'), false);
  } finally { dom.window.close(); }
});

test('a successful queue refresh recovers the same clip after an assignment refusal', async () => {
  const dom = await boot();
  try {
    Object.defineProperty(dom.window.document, 'hidden', { value: false, configurable: true });
    dom.window.eval(`
      $('text').value='keep the correction';$('text').dispatchEvent(new Event('input'));
      api=async()=>{const error=new Error('refused');error.status=409;throw error;};
    `);
    await dom.window.eval('renewLease()');
    dom.window.eval(`api=async()=>({playbackContractVersion:4,reviewer:me,items:queue});`);
    await dom.window.eval('load()');
    assert.equal(dom.window.document.getElementById('err').hidden, true);
    assert.equal(dom.window.document.getElementById('save').disabled, false);
    assert.equal(dom.window.document.getElementById('text').value, 'keep the correction');
  } finally { dom.window.close(); }
});

test('an authority-unavailable renewal stops cached playback without losing text', async () => {
  const dom = await boot();
  try {
    Object.defineProperty(dom.window.document, 'hidden', { value: false, configurable: true });
    dom.window.eval(`
      $('player').src='/api/audio/first';
      $('text').value='keep this text';$('text').dispatchEvent(new Event('input'));
      api=async()=>{const error=new Error('unavailable');error.status=503;throw error;};
    `);
    await dom.window.eval('renewLease()');
    assert.equal(dom.window.document.getElementById('player').getAttribute('src'), null);
    assert.equal(dom.window.document.getElementById('save').disabled, true);
    assert.equal(dom.window.document.getElementById('text').value, 'keep this text');
  } finally { dom.window.close(); }
});
