// Stress/smoke drive after the bug-hunt fixes: import -> 7B -> review audio (bounded) ->
// media play (DB-lock fix) while polling get_segments stays responsive -> settings round-trip ->
// error-box sweep. Reports PASS/FAIL per step.
const { chromium } = require('@playwright/test');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// No hardcoded personal path (repo hygiene): the audio to import is supplied by the environment.
const AUDIO = process.env.CORTEX_AUDIO;
if (!AUDIO) {
  console.error('Set CORTEX_AUDIO to the absolute path of the audio file to import (e.g. CORTEX_AUDIO=D:/clips/sample.wav).');
  process.exit(2);
}

(async () => {
  const b = await chromium.connectOverCDP('http://localhost:9222');
  const page = b.contexts()[0].pages().find((p) => /tauri|localhost/.test(p.url())) || b.contexts()[0].pages()[0];
  await page.waitForSelector('[data-testid="app-root"]', { timeout: 30000 });
  const results = [];
  const ok = (name, cond, extra) => { results.push(`${cond ? 'PASS' : 'FAIL'}  ${name}${extra ? ' — ' + extra : ''}`); return cond; };

  // 1) import + 7B transcribe
  await page.evaluate((p) => window.__TAURI_INTERNALS__.invoke('import_audio_file', { path: p }), AUDIO);
  let segs = [];
  for (let i = 0; i < 240; i++) {
    segs = await page.evaluate(() => window.__TAURI_INTERNALS__.invoke('get_segments', { verified: null }).catch(() => [])).catch(() => []);
    const pend = segs.filter((s) => { const t = (s.rawTranscript ?? s.raw_transcript) || ''; return !t.trim() || t.includes('Pending'); }).length;
    if (i % 5 === 0) console.log(`  import poll ${i * 2}s: ${segs.length} segs, ${pend} pending`);
    if (segs.length >= 1 && pend === 0) break;
    await sleep(2000);
  }
  ok('import + 7B transcribe', segs.length >= 1 && segs.every((s) => { const t = (s.rawTranscript ?? s.raw_transcript) || ''; return t.trim() && !t.includes('Pending'); }), `${segs.length} segs`);

  // 2) media play (register_media_asset / DB-lock fix) WHILE get_segments stays responsive
  const playRace = await page.evaluate(async () => {
    const segList = await window.__TAURI_INTERNALS__.invoke('get_segments', { verified: null }).catch(() => []);
    const path = segList[0] && (segList[0].audioPath ?? segList[0].audio_path);
    if (!path) return { ok: false, why: 'no audio path' };
    const t0 = performance.now();
    const grantP = window.__TAURI_INTERNALS__.invoke('register_media_asset', { audioPath: path });
    // immediately hammer get_segments; with the DB-lock fix these should return fast, not block on the copy
    const lat = [];
    for (let i = 0; i < 5; i++) { const s = performance.now(); await window.__TAURI_INTERNALS__.invoke('get_segments', { verified: null }).catch(() => {}); lat.push(Math.round(performance.now() - s)); }
    const grant = await grantP.catch((e) => ({ err: String(e) }));
    return { ok: !!(grant && grant.id), grantMs: Math.round(performance.now() - t0), getSegMaxMs: Math.max(...lat), lat };
  });
  ok('media grant works', playRace.ok, JSON.stringify(playRace));
  ok('get_segments stays responsive during media copy', (playRace.getSegMaxMs ?? 99999) < 2000, `max get_segments ${playRace.getSegMaxMs}ms`);

  // 3) review mode + bounded audio. review-correct-btn TOGGLES review mode, so only click it when we are
  // NOT already in review — re-clicking it every iteration would toggle back out (an even number of
  // clicks ends OUT of review and falsely fails this check).
  let rev = {};
  for (let i = 0; i < 12; i++) {
    rev = await page.evaluate(() => { const c = document.querySelector('[data-testid="center-panel"]'); const a = c?.querySelector('audio'); return { inReview: /Clip \d+ of \d+|پارچەی/.test(c?.innerText || ''), audioErr: a?.error ? a.error.code : null, ready: a?.readyState }; });
    if (rev.inReview && rev.ready === 4) break;
    if (!rev.inReview) await page.evaluate(() => document.querySelector('[data-testid="review-correct-btn"]')?.click());
    await sleep(800);
  }
  ok('review mode renders + audio loads', rev.inReview && rev.audioErr == null && rev.ready === 4, JSON.stringify(rev));

  // 4) settings round-trip (the save-surface fix path shouldn't break normal saves)
  const setRT = await page.evaluate(async () => {
    try { const s = await window.__TAURI_INTERNALS__.invoke('get_settings'); await window.__TAURI_INTERNALS__.invoke('update_settings', { settings: s }); return { ok: true }; }
    catch (e) { return { ok: false, err: String(e) }; }
  });
  ok('settings get+update round-trip', setRT.ok, setRT.err || '');

  // 5) error-box sweep
  const errs = await page.evaluate(() => ({
    boxes: [...document.querySelectorAll('div')].filter((d) => typeof d.className === 'string' && d.className.includes('bg-red-900')).length,
    undef: (document.body.innerText.match(/\bundefined\b/g) || []).length,
  }));
  ok('no error boxes / undefined', errs.boxes === 0 && errs.undef === 0, JSON.stringify(errs));

  console.log('\n==== SMOKE DRIVE ====');
  results.forEach((r) => console.log(r));
  console.log(results.every((r) => r.startsWith('PASS')) ? '\nSMOKE_OK' : '\nSMOKE_HAS_FAILURES');
  await b.close();
})().catch((e) => { console.error('SMOKE_FAIL:', e.message); process.exit(1); });
