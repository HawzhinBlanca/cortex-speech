#!/usr/bin/env node
// Verify the opt-in transcribe_segment_finetuned IPC command end-to-end on the real exe, using the
// EMBEDDED fine-tuned model (models/finetuned-mms-ckb/) — no env override — to prove the app resolves
// + loads + decodes it: import -> get_segments -> transcribe_segment_finetuned -> non-blank Kurdish.
const { spawn, execSync } = require('child_process');
const { chromium } = require('@playwright/test');
const path = require('path');
const fs = require('fs');
const REPO = __dirname;
const APP_EXE = process.env.CORTEX_APP_EXE || path.join(REPO, 'src-tauri', 'target', 'release', 'cortex-speech-app.exe');
const AUDIO = process.env.CORTEX_AUDIO;
const PORT = process.env.CORTEX_DEBUG_PORT || '9291';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
function die(m) { console.error('PRECONDITION FAILED: ' + m); process.exit(1); }
if (!AUDIO || !fs.existsSync(AUDIO)) die('CORTEX_AUDIO required (existing wav).');
if (!fs.existsSync(APP_EXE)) die('exe not found: ' + APP_EXE);
function killApp() { try { execSync('taskkill /F /IM cortex-speech-app.exe', { stdio: 'ignore' }); } catch (e) {} }
const isArabic = (s) => [...s].some((c) => (c >= '؀' && c <= 'ۿ') || (c >= 'ݐ' && c <= 'ݿ'));
(async () => {
  killApp();
  try { execSync('python clear_db.py', { cwd: REPO, stdio: 'ignore' }); } catch (e) {}
  const app = spawn(APP_EXE, [], { env: { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT}` }, cwd: path.dirname(APP_EXE) });
  app.stdout.on('data', () => {}); app.stderr.on('data', () => {});
  let ok = false;
  for (let i = 0; i < 90; i++) { try { const r = await fetch(`http://localhost:${PORT}/json`); if (r.ok) { ok = true; break; } } catch (e) {} await sleep(1000); }
  if (!ok) { killApp(); throw new Error(`debug port ${PORT} never came up`); }
  const b = await chromium.connectOverCDP(`http://localhost:${PORT}`);
  const page = b.contexts()[0].pages().find((p) => p.url().includes('localhost')) || b.contexts()[0].pages()[0];
  await page.waitForSelector('[data-testid="app-root"]', { timeout: 45000 });
  const invoke = (c, a) => page.evaluate(([cc, aa]) => window.__TAURI_INTERNALS__.invoke(cc, aa), [c, a]);
  console.log('==> import_audio_file');
  await invoke('import_audio_file', { path: AUDIO });
  let segs = [];
  for (let i = 0; i < 120; i++) { segs = (await invoke('get_segments', { verified: null }).catch(() => [])) || []; if (segs.length) break; await sleep(2000); }
  if (!segs.length) throw new Error('VAD produced 0 segments');
  const seg = segs[0];
  console.log(`==> transcribe_segment_finetuned (embedded model) on ${path.basename(seg.audioPath || '')}`);
  const res = await invoke('transcribe_segment_finetuned', { audioPath: seg.audioPath });
  const text = ((res && (res.text || res.rawTranscript)) || '').trim();
  console.log('==> fine-tuned hypothesis:', JSON.stringify(text));
  await b.close(); killApp();
  if (!text) throw new Error('fine-tuned decode returned BLANK (no-fabrication guard)');
  const nonSpace = [...text].filter((c) => !/\s/.test(c));
  if (!(nonSpace.length > 0 && nonSpace.every((c) => isArabic(c)))) throw new Error(`output left Kurdish script: ${text}`);
  console.log('\n=================================================');
  console.log(`  FINE-TUNED IPC OK (embedded model): ${text} (${text.length} chars, all Kurdish-script)`);
  console.log('=================================================');
})().catch((e) => { console.error('==> FINE-TUNED IPC FAILED:', e && e.message ? e.message : e); killApp(); process.exit(1); });
