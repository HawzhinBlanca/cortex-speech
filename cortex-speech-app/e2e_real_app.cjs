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
 *   CORTEX_APP_DATA_DIR (optional) app profile dir for THIS RUN; default: a fresh disposable temp
 *                       dir. The owner's real %APPDATA%\cortex-speech profile is REFUSED — a
 *                       verification run must be incapable of touching the production library.
 *   CORTEX_OUT         (optional) output dir for debug log + run.jsonl; default: repo root
 *   CORTEX_DEBUG_PORT  (optional) WebView2 remote-debug port; default 9222
 *   CORTEX_LOCALE      (optional) 'en' | 'ckb'; default 'en'
 *   CORTEX_ASR_ENGINE  (optional) engine to provision in the DISPOSABLE profile before import:
 *                       'CTC300M' (default: fully offline, runnable on any dev box), 'CTC1B',
 *                       'WSL7B' (needs the owner's warm 7B server + a seeded client script path —
 *                       the fresh profile's WSL7B default otherwise fail-hards the import BEFORE
 *                       any decode, and this harness would blame VAD), or 'keep' to leave the
 *                       profile's settings untouched.
 *   CORTEX_SKIP_DB_CLEAR (optional) '1' to keep existing DB rows (default: clear for a clean run)
 *
 * Exit code 0 only when: the app launched, VAD produced >=1 segment, the first segment
 * transcribed to NON-EMPTY text, and run.jsonl was written. Anything else is a hard failure.
 */
const { spawn, execSync } = require('child_process');
const { chromium } = require('@playwright/test');
const path = require('path');
const fs = require('fs');
const os = require('os');

const REPO = __dirname;
const APP_EXE = process.env.CORTEX_APP_EXE
  || path.join(REPO, 'src-tauri', 'target', 'release', 'cortex-speech-app.exe');
const AUDIO = process.env.CORTEX_AUDIO;
const OUT_DIR = process.env.CORTEX_OUT || REPO;
const DEBUG_PORT = process.env.CORTEX_DEBUG_PORT || '9222';
const LOCALE = process.env.CORTEX_LOCALE === 'ckb' ? 'ckb' : 'en';
// Clearing the DB is OPT-IN (default: keep the existing library). A verification run must never be
// able to erase the owner's real %APPDATA% library by simply being run — the old opt-out default did.
const CLEAR_DB = process.env.CORTEX_DB_CLEAR === '1' && process.env.CORTEX_SKIP_DB_CLEAR !== '1';

// ── Profile isolation (P0): this test runs against a DISPOSABLE profile, never the real library. ──
// The app honors CORTEX_APP_DATA_DIR (lib.rs get_app_data_dir), so pointing it at a temp dir gives
// the run its own DB, settings, lock, and media cache. Models still resolve via the bundled/repo
// fallback (models.rs active_models_dir), so ASR works in a fresh profile.
const PROD_PROFILE = process.env.APPDATA ? path.join(process.env.APPDATA, 'cortex-speech') : null;
const normPath = (p) => path.resolve(p).replace(/[\\/]+$/, '').toLowerCase();
let DATA_DIR = process.env.CORTEX_APP_DATA_DIR;
if (DATA_DIR) {
  if (
    PROD_PROFILE &&
    (normPath(DATA_DIR) === normPath(PROD_PROFILE) ||
      normPath(DATA_DIR).startsWith(normPath(PROD_PROFILE) + path.sep))
  ) {
    console.error(
      'REFUSED: CORTEX_APP_DATA_DIR points at the REAL profile (' + PROD_PROFILE + '). ' +
        'This harness must never run against the production library — use a disposable directory.',
    );
    process.exit(1);
  }
} else {
  DATA_DIR = fs.mkdtempSync(path.join(os.tmpdir(), 'cortex-e2e-'));
}

// The WebView2 browser profile is a SECOND shared resource, and isolating only CORTEX_APP_DATA_DIR
// left it shared. Tauri keys it on the bundle identity (%LOCALAPPDATA%\com.cortex.kurdish-speech
// \EBWebView), NOT on the data dir — so a run spawned while the owner's own Cortex is open lands in
// the SAME folder. WebView2 honours WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS only when it CREATES the
// browser process for a folder; when one already exists it fails the environment with
// HRESULT 0x8007139F (ERROR_INVALID_STATE) and --remote-debugging-port is silently dropped. This
// harness then polls for 90s on a port nobody ever opened and reports a launch timeout — a gate whose
// green depended on whether the owner happened to have the app running.
// Measured 2026-08-01: FAIL in 92.0s with the app open, PASS (real transcript) with this set.
const WEBVIEW2_DIR = process.env.WEBVIEW2_USER_DATA_FOLDER || path.join(DATA_DIR, 'webview2');
fs.mkdirSync(WEBVIEW2_DIR, { recursive: true });

// True only when THIS run minted the profile above. A caller-supplied CORTEX_APP_DATA_DIR is never
// ours to delete, whatever it points at.
const DATA_DIR_IS_OURS = !process.env.CORTEX_APP_DATA_DIR;

/// Remove the disposable profile — ONLY on success, ONLY when we created it, ONLY under the temp root.
//
// Each run leaves a full app profile (DB, media cache, settings) and, since the WebView2 isolation
// above, a ~11 MB browser profile too. Nothing ever removed them: measured 2026-08-01, 34 stale
// `cortex-e2e-*` directories totalling 764 MB on the owner's box. Isolating the browser profile made an
// existing leak grow faster, so the cleanup belongs with it.
//
// Kept on FAILURE deliberately: the profile is the only copy of the DB a post-mortem can read, and a
// gate that destroys its own evidence is worse than one that leaves a directory behind.
//
// The three guards mirror `Remove-TemporaryFixtureDir` in scripts/test-real-data.ps1 — same reasoning,
// same repo, and a recursive delete gets a guard here for the same reason it does there.
// Windows releases file handles ASYNCHRONOUSLY. `taskkill /F /T` returns as soon as the kill is
// signalled, not when the app's SQLite handles (`cortex-speech.db-wal`, `-shm`, `cortex.lock`) and its
// msedgewebview2 children have actually let go — so an immediate delete gets EPERM on the directory
// itself. Measured 2026-08-01: a single rmSync straight after killApp failed every time, and the leak it
// was written to fix stayed exactly as it was (34 dirs -> 35). Retry to a deadline instead.
async function cleanupProfile() {
  if (!DATA_DIR_IS_OURS) return;
  const root = path.resolve(os.tmpdir());
  const target = path.resolve(DATA_DIR);
  if (target === root || !target.startsWith(root + path.sep)) {
    console.log(`==> Leaving ${target} in place (outside the temp root — refusing to remove it).`);
    return;
  }
  const deadline = Date.now() + 15000;
  for (;;) {
    try {
      fs.rmSync(target, { recursive: true, force: true });
      console.log(`==> Removed the disposable profile ${target}`);
      return;
    } catch (e) {
      if (Date.now() >= deadline) {
        // Non-fatal: a leaked temp directory must never turn a passing verification run into a failure.
        console.log(`==> Could not remove the disposable profile ${target}: ${e.message}`);
        return;
      }
      await sleep(500);
    }
  }
}

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

// Kill ONLY the process tree we spawned (never `taskkill /IM` by image name, which would also kill
// the owner's own running Cortex). No-op when nothing was spawned.
let appProcess = null;
function killApp() {
  if (appProcess && appProcess.pid) {
    try { execSync(`taskkill /F /T /PID ${appProcess.pid}`, { stdio: 'ignore' }); } catch (e) {}
    appProcess = null;
  }
}

function dumpRunManifest() {
  // Export the imported/transcribed segments to run.jsonl for build_review_page.py.
  // Honest by construction: it copies exactly what THIS RUN's isolated DB holds (never the
  // production %APPDATA% database).
  const out = path.join(OUT_DIR, 'run.jsonl').replace(/\\/g, '\\\\');
  const dbPath = path.join(DATA_DIR, 'cortex-speech.db').replace(/\\/g, '\\\\');
  const py = [
    'import sqlite3, os, json, sys',
    "db=r'" + dbPath + "'",
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
  console.log(`==> Isolated profile for this run: ${DATA_DIR}`);
  console.log(`==> Isolated WebView2 browser profile: ${WEBVIEW2_DIR}`);
  // A STALE debug-port owner (e.g. a previous run that leaked) would make us drive the wrong
  // instance. Never kill by image name — detect and refuse instead.
  const portInUse = await fetch(`http://localhost:${DEBUG_PORT}/json`).then((r) => r.ok, () => false);
  if (portInUse) {
    die(
      `debug port ${DEBUG_PORT} is already answering — a previous instance is still running. ` +
        'Close it (or set CORTEX_DEBUG_PORT to a free port); this harness only kills processes it spawned.',
    );
  }
  if (CLEAR_DB) {
    console.log('==> Clearing the ISOLATED profile DB for a clean run (CORTEX_DB_CLEAR=1; snapshots first)...');
    // clear_db.py honors CORTEX_APP_DATA_DIR, snapshots first, and refuses without this confirm.
    try {
      execSync('python clear_db.py --yes', {
        cwd: REPO,
        env: { ...process.env, CORTEX_APP_DATA_DIR: DATA_DIR, CORTEX_DB_CLEAR_CONFIRM: '1' },
      });
    } catch (e) { console.log('   db clear skipped:', e.message); }
  }

  console.log(`==> Launching ${path.basename(APP_EXE)} with remote-debugging-port=${DEBUG_PORT}...`);
  appProcess = spawn(APP_EXE, [], {
    env: {
      ...process.env,
      CORTEX_APP_DATA_DIR: DATA_DIR,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${DEBUG_PORT}`,
      WEBVIEW2_USER_DATA_FOLDER: WEBVIEW2_DIR,
    },
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

  // Provision a RUNNABLE engine in the disposable profile. A fresh profile boots with the
  // WSL7B default + no client script, so import fail-hards at the engine-unresolved gate BEFORE
  // any decode — and this harness would then poll get_segments for 12 minutes and misreport the
  // failure as "VAD produced 0 segments" (2026-07-11 root-cause). Round-trip the real settings
  // object so only the engine field changes.
  const ENGINE = process.env.CORTEX_ASR_ENGINE || 'CTC300M';
  if (ENGINE !== 'keep') {
    console.log(`==> Provisioning ASR engine '${ENGINE}' in the disposable profile...`);
    await page.evaluate(async (engine) => {
      const s = await window.__TAURI_INTERNALS__.invoke('get_settings');
      s.asr_model_size = engine;
      await window.__TAURI_INTERNALS__.invoke('update_settings', { settings: s });
    }, ENGINE).catch((e) => { throw new Error('Could not provision the ASR engine: ' + e.message); });
  }

  console.log('==> Importing real audio:', AUDIO);
  // Surface an import rejection IMMEDIATELY (engine preflight, unreadable file) instead of letting
  // the segment poll below time out and blame VAD for an import that never started.
  await page
    .evaluate((p) => window.__TAURI_INTERNALS__.invoke('import_audio_file', { path: p }), AUDIO)
    .catch((e) => { throw new Error('import_audio_file failed: ' + (e && e.message ? e.message : e)); });

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
    // Do NOT blame VAD: an import that failed asynchronously (engine unresolved/preflight, decode
    // error) also leaves get_segments empty. Report what is actually known.
    throw new Error(
      'no segments appeared within the 12-min window (backend get_segments empty) — the import ' +
        'failed or never persisted. Check the [App] log above for the import error; a fresh ' +
        "profile needs a runnable engine (see CORTEX_ASR_ENGINE, default 'CTC300M').",
    );
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
  // After killApp: the profile is only removable once the app has released its lock and DB handles.
  await cleanupProfile();
  console.log('\n=================================================');
  console.log(`  REAL-DATA RUN OK: ${segCount} segments; first transcript ${rawText.length} chars`);
  console.log('  Next: python scripts/build_review_page.py --manifest run.jsonl --out review.html --embed-audio');
  console.log('=================================================');
}

run().catch((err) => {
  console.error('==> REAL-DATA RUN FAILED:', err && err.message ? err.message : err);
  killApp();
  // NOT cleaned up on failure — see cleanupProfile. Say where it is so the post-mortem can find it.
  console.error(`==> Profile kept for diagnosis: ${DATA_DIR}`);
  process.exit(1);
});
