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

const REPO = __dirname;
const APP_EXE = process.env.CORTEX_APP_EXE
  || path.join(REPO, 'src-tauri', 'target', 'release', 'cortex-speech-app.exe');
const AUDIO = process.env.CORTEX_AUDIO;
const DEBUG_PORT = process.env.CORTEX_DEBUG_PORT || '9222';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
function die(m) { console.error('PRECONDITION FAILED: ' + m); process.exit(1); }
if (!AUDIO) die('CORTEX_AUDIO is required (absolute path to a real audio file).');
if (!fs.existsSync(AUDIO)) die('CORTEX_AUDIO does not exist: ' + AUDIO);
if (!fs.existsSync(APP_EXE)) die('App exe not found: ' + APP_EXE);
function killApp() { try { execSync('taskkill /F /IM cortex-speech-app.exe', { stdio: 'ignore' }); } catch (e) {} }

async function run() {
  killApp();
  try { execSync('python clear_db.py', { cwd: REPO, stdio: 'ignore' }); } catch (e) {}
  console.log(`==> Launching ${path.basename(APP_EXE)} (remote-debug ${DEBUG_PORT})...`);
  const app = spawn(APP_EXE, [], {
    env: { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${DEBUG_PORT}` },
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
  console.log('\n=================================================');
  console.log(`  PIPELINE PROOF OK: import -> VAD (${segs.length} seg) -> ASR -> "${raw}" (${raw.length} chars)`);
  console.log('=================================================');
}
run().catch((e) => { console.error('==> PIPELINE PROOF FAILED:', e && e.message ? e.message : e); killApp(); process.exit(1); });
