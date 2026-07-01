// Connect-only e2e: the app is ALREADY running with --remote-debugging-port (launched from PowerShell,
// which reliably propagates WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS). We just attach over CDP, import the
// audio via IPC, and poll get_segments until the app's own WSL-7B import pass transcribes every segment.
const { chromium } = require('@playwright/test');

// No hardcoded personal path (repo hygiene): the audio to import is supplied by the environment.
const AUDIO = process.env.CORTEX_AUDIO;
if (!AUDIO) {
  console.error('Set CORTEX_AUDIO to the absolute path of the audio file to import (e.g. CORTEX_AUDIO=D:/clips/sample.wav).');
  process.exit(2);
}
const PORT = process.env.CORTEX_DEBUG_PORT || '9222';
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
