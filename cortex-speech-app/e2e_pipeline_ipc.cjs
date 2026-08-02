#!/usr/bin/env node
/**
 * e2e_pipeline_ipc.cjs -- headless PIPELINE smoke against the REAL Cortex desktop app.
 *
 * Drives the real .exe's Rust backend over CDP using the SAME Tauri IPC commands the UI buttons
 * call (import_audio_file -> get_segments -> transcribe_segment), and asserts a NON-BLANK
 * transcript (no-fabrication guard). This proves import -> VAD -> ASR end-to-end on the real
 * binary independent of webview rendering quirks (VirtualList measurement, event-channel timing),
 * which the UI driver e2e_real_app.cjs exercises separately.
 *
 * Env: CORTEX_AUDIO (required), CORTEX_APP_EXE (optional), CORTEX_DEBUG_PORT (optional, 9222).
 * Exit 0 only when a segment is produced AND transcribe_segment returns non-empty text.
 */
const { spawn, execSync } = require('child_process');
const { chromium } = require('@playwright/test');
const path = require('path');
const fs = require('fs');
const { resolveDisposableProfile, launchEnv, killSpawned, cleanupProfile, refuseIfDebugPortBusy, provisionEngine } = require('./e2e_profile.cjs');

const REPO = __dirname;
const APP_EXE = process.env.CORTEX_APP_EXE
  || path.join(REPO, 'src-tauri', 'target', 'release', 'cortex-speech-app.exe');
const DEFAULT_AUDIO = path.join(REPO, 'src-tauri', 'tests', 'fixtures', 'fleurs_ckb_sample.wav');
// Defaults to the committed FLEURS ckb fixture so this can run as a GATE rather than only by hand.
// Requiring the env var is why nothing ever ran it. CORTEX_AUDIO still overrides.
const AUDIO = process.env.CORTEX_AUDIO || DEFAULT_AUDIO;
// PRIVATE, not 9222 — see e2e_real_app.cjs for the full reasoning. Short version: 9222 is the port
// every other dev tool grabs by default (the Antigravity IDE's browser holds it permanently on this
// machine), so this leg refused three sweeps in a row without testing anything.
const DEBUG_PORT = process.env.CORTEX_DEBUG_PORT || '9261';
// DISPOSABLE profile, never the owner's library — see e2e_profile.cjs. Resolved before any
// spawn so there is no window in which this harness could launch against %APPDATA%.
const { dataDir: DATA_DIR, ours: DATA_DIR_IS_OURS } = resolveDisposableProfile('e2e_pipeline_ipc');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
function die(m) { console.error('PRECONDITION FAILED: ' + m); process.exit(1); }
if (!fs.existsSync(AUDIO)) die('CORTEX_AUDIO does not exist: ' + AUDIO);
if (!fs.existsSync(APP_EXE)) die('App exe not found: ' + APP_EXE);
let appProcess = null;
function killApp() { killSpawned(appProcess); }

async function run() {
  await refuseIfDebugPortBusy(DEBUG_PORT, 'e2e_pipeline_ipc');
  try {
    // Against the DISPOSABLE profile, with the confirmation clear_db.py demands. Previously
    // this ran with neither, so it was refused (exit 2) and swallowed — a clean-slate step
    // that never once cleaned anything.
    execSync('python clear_db.py --yes', {
      cwd: REPO, stdio: 'ignore',
      env: { ...process.env, CORTEX_APP_DATA_DIR: DATA_DIR, CORTEX_DB_CLEAR_CONFIRM: '1' },
    });
  } catch (e) { /* a fresh profile has no DB yet, which is the normal case */ }
  console.log(`==> Launching ${path.basename(APP_EXE)} (remote-debug ${DEBUG_PORT})...`);
  const app = appProcess = spawn(APP_EXE, [], {
    env: launchEnv(DATA_DIR, DEBUG_PORT),
    cwd: path.dirname(APP_EXE), shell: false,
  });
  // Drain BOTH stdout and stderr: the app logs heavily and an undrained pipe buffer (~64 KB)
  // will block the process mid-startup so the remote-debug port never opens.
  app.stdout.on('data', () => {});
  app.stderr.on('data', (d) => { const s = d.toString().trim(); if (s) console.error('[App:err]', s); });

  let pages = null;
  for (let i = 0; i < 90; i++) {
    try { const r = await fetch(`http://localhost:${DEBUG_PORT}/json`); if (r.ok) { pages = await r.json(); break; } } catch (e) {}
    await sleep(1000);
  }
  if (!pages) { killApp(); throw new Error(`debug port ${DEBUG_PORT} never came up`); }

  const browser = await chromium.connectOverCDP(`http://localhost:${DEBUG_PORT}`);
  const ctx = browser.contexts()[0];
  const page = ctx.pages().find((p) => p.url().includes('localhost')) || ctx.pages()[0];
  await page.waitForSelector('[data-testid="app-root"]', { timeout: 45000 });
  await provisionEngine(page);
  const invoke = (cmd, args) => page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args]);

  console.log('==> import_audio_file:', AUDIO);
  await invoke('import_audio_file', { path: AUDIO });

  console.log('==> Polling get_segments for VAD output...');
  let segs = [];
  for (let i = 0; i < 360; i++) {
    segs = (await invoke('get_segments', { verified: null }).catch(() => [])) || [];
    if (Array.isArray(segs) && segs.length >= 1) break;
    await sleep(2000);
  }
  if (!segs.length) throw new Error('VAD produced 0 segments (get_segments empty).');
  console.log(`==> VAD produced ${segs.length} segment(s).`);

  const seg = segs[0];
  console.log(`==> transcribe_segment on segment ${seg.id} (audio=${path.basename(seg.audioPath || '')})...`);
  const result = await invoke('transcribe_segment', { segmentId: seg.id, audioPath: seg.audioPath, alignmentJson: seg.alignmentJson || null });
  const raw = ((result && (result.rawTranscript || result.text)) || '').trim();
  console.log('==> Model hypothesis:', JSON.stringify(raw));

  if (!raw) throw new Error('ASR returned a BLANK transcript (no-fabrication guard).');

  await browser.close();
  killApp();
  await cleanupProfile(DATA_DIR, DATA_DIR_IS_OURS);
  console.log('\n=================================================');
  console.log(`  PIPELINE PROOF OK: import -> VAD (${segs.length} seg) -> ASR -> "${raw}" (${raw.length} chars)`);
  console.log('=================================================');
}
run().catch((e) => { console.error('==> PIPELINE PROOF FAILED:', e && e.message ? e.message : e); killApp(); process.exit(1); });
