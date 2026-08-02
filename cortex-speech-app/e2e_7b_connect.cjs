// Connect-only e2e: the app is ALREADY running with --remote-debugging-port (launched from PowerShell,
// which reliably propagates WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS). We just attach over CDP, import the
// audio via IPC, and poll get_segments until the app's own WSL-7B import pass transcribes every segment.
const { chromium } = require('@playwright/test');

// THIS ONE WRITES TO THE LIVE LIBRARY, and unlike its siblings it cannot be given a disposable
// profile: it launches nothing. It ATTACHES to an already-running app and imports into whatever
// library that app has open — normally the real %APPDATA%\cortex-speech one, holding the owner's
// human review decisions. That is the harness's whole purpose (it drives his real setup against the
// WSL-7B server), so the fix is not isolation but an explicit acknowledgement: a casual run must not
// be able to quietly add clips to the corpus.
//
// e2e_constrained_ipc / e2e_finetuned_ipc / e2e_pipeline_ipc got disposable profiles instead, via
// e2e_profile.cjs. Same hazard, different remedy, because those three spawn their own app.
if (process.env.CORTEX_ALLOW_LIVE_PROFILE !== '1') {
  console.error(
    'REFUSED: e2e_7b_connect attaches to an ALREADY-RUNNING app and imports into the library that app\n' +
      '  has open — normally the REAL one. It cannot use a disposable profile because it launches\n' +
      '  nothing. Set CORTEX_ALLOW_LIVE_PROFILE=1 to confirm you mean to add clips to that library,\n' +
      '  or use e2e_pipeline_ipc.cjs, which does the same import->VAD->ASR proof in isolation.',
  );
  process.exit(1);
}

// No hardcoded personal path (repo hygiene): the audio to import is supplied by the environment.
const AUDIO = process.env.CORTEX_AUDIO;
if (!AUDIO) {
  console.error('Set CORTEX_AUDIO to the absolute path of the audio file to import (e.g. CORTEX_AUDIO=D:/clips/sample.wav).');
  process.exit(2);
}
// PRIVATE, not 9222 — same latent collision the two verify-10 e2e harnesses hit (see
// e2e_real_app.cjs). This one is not a gate leg, so nothing had exposed it yet; fixed here rather than
// left as the last copy of a bug whose two siblings were just repaired.
const PORT = process.env.CORTEX_DEBUG_PORT || '9251';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const txt = (s) => (s && (s.rawTranscript ?? s.raw_transcript)) || '';
const dur = (s) => (s && (s.durationMs ?? s.duration_ms)) || 0;

(async () => {
  const b = await chromium.connectOverCDP(`http://localhost:${PORT}`);
  const ctx = b.contexts()[0];
  const page = ctx.pages().find((p) => /localhost|1420|tauri/.test(p.url())) || ctx.pages()[0];
  await page.waitForSelector('[data-testid="app-root"]', { timeout: 45000 });
  console.log('connected to app:', page.url());

  console.log('importing:', AUDIO);
  const imp = await page.evaluate((p) => window.__TAURI_INTERNALS__.invoke('import_audio_file', { path: p }).then(() => 'ok').catch((e) => 'ERR:' + e), AUDIO);
  console.log('import returned:', imp);

  console.log('polling get_segments until every segment has a 7B transcript (up to 8 min)...');
  let segs = [];
  for (let i = 0; i < 240; i++) {
    segs = await page.evaluate(() => window.__TAURI_INTERNALS__.invoke('get_segments', { verified: null }).catch(() => [])).catch(() => []);
    const pend = segs.filter((s) => !txt(s).trim() || txt(s).includes('Pending')).length;
    if (segs.length >= 1 && pend === 0) break;
    if (i % 8 === 0) console.log(`  ${segs.length} segs, ${pend} pending/blank... ${i * 2}s`);
    await sleep(2000);
  }

  console.log(`\n==== RESULT: ${segs.length} segment(s) ====`);
  for (const s of segs) console.log(`[${Math.round(dur(s) / 1000)}s] ${txt(s)}`);
  const allGood = segs.length >= 1 && segs.every((s) => txt(s).trim() && !txt(s).includes('Pending'));
  console.log('\n' + (allGood ? 'E2E_OK: the running app imported and 7B-transcribed every segment.' : 'E2E_INCOMPLETE: some segments never got a 7B transcript.'));
  await b.close();
  process.exit(allGood ? 0 : 2);
})().catch((e) => { console.error('E2E_FAIL:', e && e.message ? e.message : e); process.exit(1); });
