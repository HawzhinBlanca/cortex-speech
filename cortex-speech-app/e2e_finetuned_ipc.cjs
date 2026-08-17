#!/usr/bin/env node
// Verify the opt-in transcribe_segment_finetuned IPC command end-to-end on the real exe, using the
// EMBEDDED fine-tuned model (models/finetuned-mms-ckb/) — no env override — to prove the app resolves
// + loads + decodes it: import -> get_segments -> transcribe_segment_finetuned -> non-blank Kurdish.
const { spawn, execSync } = require('child_process');
const { chromium } = require('@playwright/test');
const path = require('path');
const fs = require('fs');
const { resolveDisposableProfile, launchEnv, killSpawned, cleanupProfile, refuseIfDebugPortBusy, provisionEngine } = require('./e2e_profile.cjs');
const REPO = __dirname;
const APP_EXE = process.env.CORTEX_APP_EXE || path.join(REPO, 'src-tauri', 'target', 'release', 'cortex-speech-app.exe');
const DEFAULT_AUDIO = path.join(REPO, 'src-tauri', 'tests', 'fixtures', 'fleurs_ckb_sample.wav');
// Defaults to the committed FLEURS ckb fixture so this can run as a GATE rather than only by hand.
// Requiring the env var is why nothing ever ran it. CORTEX_AUDIO still overrides.
const AUDIO = process.env.CORTEX_AUDIO || DEFAULT_AUDIO;
const PORT = process.env.CORTEX_DEBUG_PORT || '9291';
// DISPOSABLE profile, never the owner's library — see e2e_profile.cjs. Resolved before any
// spawn so there is no window in which this harness could launch against %APPDATA%.
const { dataDir: DATA_DIR, ours: DATA_DIR_IS_OURS } = resolveDisposableProfile('e2e_finetuned_ipc');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
function die(m) { console.error('PRECONDITION FAILED: ' + m); process.exit(1); }
if (!AUDIO || !fs.existsSync(AUDIO)) die('audio not found: ' + AUDIO);
if (!fs.existsSync(APP_EXE)) die('exe not found: ' + APP_EXE);
let appProcess = null;
function killApp() { killSpawned(appProcess); }
// The failure mode worth catching is the model emitting LATIN text — an English hypothesis leaking
// out of a Kurdish decode. The old check demanded EVERY non-space character be Arabic-script, which
// rejects the committed ground truth itself: tests/fixtures/fleurs_ckb_sample.txt reads
// "…ساڵی 1800ــەوە…", and 1/8/0 plus a left-to-right mark all fail it. A gate that rejects its own
// reference transcript is broken, not strict — proven by running the old predicate over that file.
// e2e_constrained_ipc passed only because CTC-300M happened to emit Arabic-Indic ١; the fine-tuned
// model emits Western digits, exactly as the reference does.
const hasArabic = (s) => [...s].some((c) => (c >= '؀' && c <= 'ۿ') || (c >= 'ݐ' && c <= 'ݿ'));
const hasLatinLetters = (s) => /[A-Za-z]/.test(s);
(async () => {
  await refuseIfDebugPortBusy(PORT, 'e2e_finetuned_ipc');
  try {
    // Against the DISPOSABLE profile, with the confirmation clear_db.py demands. Previously
    // this ran with neither, so it was refused (exit 2) and swallowed — a clean-slate step
    // that never once cleaned anything.
    execSync('python clear_db.py --yes', {
      cwd: REPO, stdio: 'ignore',
      env: { ...process.env, CORTEX_APP_DATA_DIR: DATA_DIR, CORTEX_DB_CLEAR_CONFIRM: '1' },
    });
  } catch (e) { /* a fresh profile has no DB yet, which is the normal case */ }
  const app = appProcess = spawn(APP_EXE, [], { env: launchEnv(DATA_DIR, PORT), cwd: path.dirname(APP_EXE) });
  app.stdout.on('data', () => {}); app.stderr.on('data', () => {});
  let ok = false;
  for (let i = 0; i < 90; i++) { try { const r = await fetch(`http://localhost:${PORT}/json`); if (r.ok) { ok = true; break; } } catch (e) {} await sleep(1000); }
  if (!ok) { killApp(); throw new Error(`debug port ${PORT} never came up`); }
  const b = await chromium.connectOverCDP(`http://localhost:${PORT}`);
  const page = b.contexts()[0].pages().find((p) => p.url().includes('localhost')) || b.contexts()[0].pages()[0];
  await page.waitForSelector('[data-testid="app-root"]', { timeout: 45000 });
  await provisionEngine(page);
  const invoke = (c, a) => page.evaluate(([cc, aa]) => window.__TAURI_INTERNALS__.invoke(cc, aa), [c, a]);
  console.log('==> import_audio_file');
  await invoke('import_audio_file', { path: AUDIO });
  let segs = [];
  for (let i = 0; i < 120; i++) { segs = (await invoke('get_segments_page', { verified: null, query: null, sort: 'oldest', limit: 300, cursor: null }).then((p) => p.items).catch((e) => { if (String(e).includes('not found')) throw e; return []; })) || []; if (segs.length) break; await sleep(2000); }
  if (!segs.length) throw new Error('VAD produced 0 segments');
  const seg = segs[0];
  console.log(`==> transcribe_segment_finetuned (embedded model) on ${path.basename(seg.audioPath || '')}`);
  const res = await invoke('transcribe_segment_finetuned', { audioPath: seg.audioPath });
  const text = ((res && (res.text || res.rawTranscript)) || '').trim();
  console.log('==> fine-tuned hypothesis:', JSON.stringify(text));
  await b.close(); killApp();
  await cleanupProfile(DATA_DIR, DATA_DIR_IS_OURS);
  if (!text) throw new Error('fine-tuned decode returned BLANK (no-fabrication guard)');
  if (!hasArabic(text)) throw new Error(`fine-tuned output has no Kurdish script at all: ${text}`);
  if (hasLatinLetters(text)) throw new Error(`fine-tuned output leaked Latin text: ${text}`);
  console.log('\n=================================================');
  console.log(`  FINE-TUNED IPC OK (embedded model): ${text} (${text.length} chars, Kurdish script, no Latin)`);
  console.log('=================================================');
})().catch((e) => { console.error('==> FINE-TUNED IPC FAILED:', e && e.message ? e.message : e); killApp(); process.exit(1); });
