#!/usr/bin/env node
/**
 * e2e_real_app.cjs -- drive the REAL Cortex desktop app like a user, on real audio.
 *
 * Hardened: parameterized, idempotent, with a NO-FABRICATION guard (fails on a blank ASR
 * transcript) and a run.jsonl export that feeds scripts/build_review_page.py.
 *
 * Environment:
 *   CORTEX_AUDIO       (required) absolute path to a real audio file to import
 *   CORTEX_APP_EXE     (optional) path to cortex-speech-app.exe; default: repo release build
 *   CORTEX_OUT         (optional) output dir for debug log + run.jsonl; default: repo root
 *   CORTEX_DEBUG_PORT  (optional) WebView2 remote-debug port; default 9222
 *   CORTEX_LOCALE      (optional) 'en' | 'ckb'; default 'en'
 *   CORTEX_SKIP_DB_CLEAR (optional) '1' to keep existing DB rows (default: clear for a clean run)
 *
 * Exit code 0 only when: the app launched, VAD produced >=1 segment, the first segment
 * transcribed to NON-EMPTY text, and run.jsonl was written. Anything else is a hard failure.
 */
const { spawn, execSync } = require('child_process');
const { chromium } = require('@playwright/test');
const path = require('path');
const fs = require('fs');

const REPO = __dirname;
const APP_EXE = process.env.CORTEX_APP_EXE
  || path.join(REPO, 'src-tauri', 'target', 'release', 'cortex-speech-app.exe');
const AUDIO = process.env.CORTEX_AUDIO;
const OUT_DIR = process.env.CORTEX_OUT || REPO;
const DEBUG_PORT = process.env.CORTEX_DEBUG_PORT || '9222';
const LOCALE = process.env.CORTEX_LOCALE === 'ckb' ? 'ckb' : 'en';
const SKIP_DB_CLEAR = process.env.CORTEX_SKIP_DB_CLEAR === '1';

const logFile = path.join(OUT_DIR, 'e2e_real_app_debug.log');
try { fs.writeFileSync(logFile, ''); } catch (e) { /* non-fatal */ }
const tee = (orig, tag) => (...args) => {
  const msg = args.map((a) => (typeof a === 'object' ? JSON.stringify(a) : a)).join(' ');
  try { fs.appendFileSync(logFile, `[${tag}] ${new Date().toISOString()} ${msg}\n`); } catch (e) {}
  orig.apply(console, args);
};
console.log = tee(console.log.bind(console), 'LOG');
console.error = tee(console.error.bind(console), 'ERR');

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
function die(msg) { console.error('PRECONDITION FAILED: ' + msg); process.exit(1); }

if (!AUDIO) die('CORTEX_AUDIO is required (absolute path to a real audio file).');
if (!fs.existsSync(AUDIO)) die('CORTEX_AUDIO does not exist: ' + AUDIO);
if (!fs.existsSync(APP_EXE)) die('App exe not found: ' + APP_EXE + ' (build it or set CORTEX_APP_EXE).');

function killApp() { try { execSync('taskkill /F /IM cortex-speech-app.exe', { stdio: 'ignore' }); } catch (e) {} }

function dumpRunManifest() {
  // Export the imported/transcribed segments to run.jsonl for build_review_page.py.
  // Honest by construction: it copies exactly what the DB holds (empty raw_transcript stays empty).
  const out = path.join(OUT_DIR, 'run.jsonl').replace(/\\/g, '\\\\');
  const py = [
    'import sqlite3, os, json, sys',
    "db=os.path.expandvars(r'%APPDATA%\\\\cortex-speech\\\\cortex-speech.db')",
    'c=sqlite3.connect(db)',
    "rows=c.execute('SELECT id,audio_path,raw_transcript,duration_ms,speaker_id FROM speech_segments ORDER BY created_at').fetchall()",
    'c.close()',
    "f=open(r'" + out + "','w',encoding='utf-8')",
    "[f.write(json.dumps({'id':r[0],'audio_path':r[1],'raw_transcript':r[2] or '','duration_ms':r[3] or 0,'speaker_id':r[4] or ''},ensure_ascii=False)+chr(10)) for r in rows]",
    'f.close()',
    "print(len(rows))",
  ].join('; ');
  const n = execSync(`python -c "${py}"`, { cwd: REPO }).toString().trim();
  console.log(`==> Wrote run.jsonl with ${n} segments -> ${path.join(OUT_DIR, 'run.jsonl')}`);
}

async function run() {
  console.log('==> Cleaning up previous app instances...');
  killApp();
  if (!SKIP_DB_CLEAR) {
    console.log('==> Clearing database for a clean run...');
    try { execSync('python clear_db.py', { cwd: REPO }); } catch (e) { console.log('   db clear skipped:', e.message); }
  }

  console.log(`==> Launching ${path.basename(APP_EXE)} with remote-debugging-port=${DEBUG_PORT}...`);
  const appProcess = spawn(APP_EXE, [], {
    env: { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${DEBUG_PORT}` },
    cwd: path.dirname(APP_EXE), shell: false, detached: false,
  });
  appProcess.stdout.on('data', (d) => console.log(`[App] ${d.toString().trim()}`));
  appProcess.stderr.on('data', (d) => console.error(`[App:err] ${d.toString().trim()}`));

  console.log(`==> Polling http://localhost:${DEBUG_PORT}/json ...`);
  let pages = null;
  for (let i = 0; i < 90; i++) {
    try { const res = await fetch(`http://localhost:${DEBUG_PORT}/json`); if (res.ok) { pages = await res.json(); break; } } catch (e) {}
    await sleep(1000);
  }
  if (!pages) { killApp(); throw new Error(`WebView2 debug port ${DEBUG_PORT} did not come up within 90s.`); }

  const browser = await chromium.connectOverCDP(`http://localhost:${DEBUG_PORT}`);
  const ctx = browser.contexts()[0];
  let page = ctx.pages().find((p) => p.url().includes('localhost') || p.url().includes('1420')) || ctx.pages()[0];
  page.on('console', (m) => console.log(`[ui:${m.type()}] ${m.text()}`));
  page.on('pageerror', (e) => console.error(`[ui:error] ${e.message}`));

  await page.waitForSelector('[data-testid="app-root"]', { timeout: 45000 });
  console.log('==> App shell rendered. Setting locale to', LOCALE);
  await page.evaluate((loc) => { localStorage.setItem('cortex-locale', loc); window.location.reload(); }, LOCALE);
  await page.waitForSelector('[data-testid="app-root"]', { timeout: 30000 });

  console.log('==> Importing real audio:', AUDIO);
  await page.evaluate((p) => window.__TAURI_INTERNALS__.invoke('import_audio_file', { path: p }), AUDIO);

  // Poll the BACKEND for segments (ground truth) rather than relying on the UI auto-refreshing
  // via the Tauri event channel, which can be delivered unreliably under remote-debugging. Once
  // the backend has the segment, reload so the UI renders it from the DB in a clean (settled) state.
  console.log('==> Waiting for Silero VAD segmentation (polling backend get_segments, up to 12 min)...');
  let backendSegs = [];
  for (let i = 0; i < 360; i++) {
    backendSegs = await page
      .evaluate(() => window.__TAURI_INTERNALS__.invoke('get_segments', { verified: null }).catch(() => []))
      .catch(() => []);
    if (Array.isArray(backendSegs) && backendSegs.length >= 1) break;
    if (i % 15 === 0) console.log(`   still segmenting... ${i * 2}s`);
    await sleep(2000);
  }
  if (!Array.isArray(backendSegs) || backendSegs.length === 0) {
    throw new Error('VAD produced 0 segments for the provided audio (backend get_segments empty).');
  }
  console.log(`==> VAD produced ${backendSegs.length} segment(s) (backend). Reloading UI to render from DB...`);
  await page.reload();
  await page.waitForSelector('[data-testid="app-root"]', { timeout: 30000 });
  await page.locator('[data-testid="segment-card"]').first().waitFor({ state: 'visible', timeout: 60000 });
  const segCount = await page.locator('[data-testid="segment-card"]').count();
  console.log(`==> UI rendered ${segCount} segment-card(s).`);

  console.log('==> Selecting first segment and running local ASR (OmniASR CTC)...');
  await page.locator('[data-testid="segment-card"]').first().click();
  await page.waitForTimeout(800);

  // The per-segment Transcribe button is disabled while the import/jury pipeline is still
  // processing ($isProcessing). Wait for it to settle before clicking, and target the precise
  // segment button (data-testid) rather than a substring match that also hits "Transcribe Empty".
  const transcribeBtn = page.locator('[data-testid="transcribe-btn"]');
  await transcribeBtn.waitFor({ state: 'visible', timeout: 60000 });
  let settled = false;
  for (let i = 0; i < 150; i++) { // up to 5 min for post-import adjudication to finish
    if (await transcribeBtn.isEnabled().catch(() => false)) { settled = true; break; }
    if (i % 10 === 0) console.log(`   waiting for pipeline to settle (transcribe enabled)... ${i * 2}s`);
    await sleep(2000);
  }
  if (!settled) throw new Error('Transcribe button never became enabled (import/jury pipeline did not settle).');
  await transcribeBtn.click();
  await page.waitForTimeout(3000);

  // A transcript still wrapped in brackets ("[Pending WSL 7B ASR]", "[Transcribing…]") is an
  // in-progress PLACEHOLDER, not real output — the 7B call has not landed yet. Treating it as the
  // transcript was a false green (it slipped past the blank-only guard and reported a placeholder as
  // success). Keep polling PAST the placeholder for the real text; if it never resolves, fail honestly.
  const isPlaceholder = (t) => /^\[.*\]$/.test(t) || /pending|transcrib/i.test(t);
  let rawText = '';
  for (let i = 0; i < 150; i++) { // up to 5 min: a cold 7B segment can take minutes
    rawText = ((await page.locator('#raw-ts').inputValue().catch(() => '')) || '').trim();
    if (rawText && !isPlaceholder(rawText)) break;
    if (i % 15 === 0 && rawText) console.log(`   still waiting for real ASR (placeholder: ${JSON.stringify(rawText)})... ${i * 2}s`);
    await sleep(2000);
  }
  console.log('==> Model hypothesis:', JSON.stringify(rawText));

  // NO-FABRICATION GUARD: a blank OR still-placeholder transcript is a real failure, not something to
  // paper over. Reporting "[Pending WSL 7B ASR]" as a transcript is exactly the fabrication this guards.
  if (!rawText) throw new Error('ASR returned a BLANK transcript (no-fabrication guard) -- refusing to report success.');
  if (isPlaceholder(rawText)) {
    throw new Error(`ASR never resolved past the placeholder ${JSON.stringify(rawText)} (7B did not complete) -- refusing to report a placeholder as success.`);
  }

  dumpRunManifest();

  console.log('==> Closing app...');
  await browser.close();
  killApp();
  console.log('\n=================================================');
  console.log(`  REAL-DATA RUN OK: ${segCount} segments; first transcript ${rawText.length} chars`);
  console.log('  Next: python scripts/build_review_page.py --manifest run.jsonl --out review.html --embed-audio');
  console.log('=================================================');
}

run().catch((err) => { console.error('==> REAL-DATA RUN FAILED:', err && err.message ? err.message : err); killApp(); process.exit(1); });
