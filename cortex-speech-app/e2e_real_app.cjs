#!/usr/bin/env node
/**
 * e2e_real_app.cjs -- drive the REAL Cortex desktop app like a user, on real audio.
 *
 * Hardened: parameterized, idempotent, with a NO-FABRICATION guard (fails on a blank ASR
 * transcript) and a run.jsonl export that feeds scripts/build_review_page.py.
 *
 * Environment:
 *   CORTEX_AUDIO       (optional) absolute path to a real audio file to import; default: the
 *                       committed FLEURS ckb fixture, so this gate runs instead of skipping
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
// Defaults to the committed FLEURS ckb fixture — real Kurdish speech, attribution beside it in
// tests/fixtures/ATTRIBUTION.md, and the same file the RTF gate measures on. Requiring the env var
// made THE daily-use reliability gate report SKIP-ENV whenever an operator forgot it, which is
// precisely when you most want it to have run: verify-10 came back "22 PASS, 0 FAIL" with this leg
// silently not executed. Set CORTEX_AUDIO to drive it with your own audio instead; either way the
// path actually used is printed before the import.
const DEFAULT_AUDIO = path.join(REPO, 'src-tauri', 'tests', 'fixtures', 'fleurs_ckb_sample.wav');
const AUDIO = process.env.CORTEX_AUDIO || DEFAULT_AUDIO;
const OUT_DIR = process.env.CORTEX_OUT || REPO;
const DEBUG_PORT = process.env.CORTEX_DEBUG_PORT || '9222';
// Deliberately NOT the real 8737 — see the spawn env below.
const COUCH_PORT = Number(process.env.CORTEX_COUCH_PORT || 18737);
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
// THE ONE recursive delete in this file, inseparable from its three guards. It is a function rather
// than inline code because two callers need it with DIFFERENT retry policies — cleanupProfile has to
// wait out Windows' asynchronous handle release, die() has nothing to wait for — and the alternative
// was a second `fs.rmSync` with its own copy of the guards. `test_real_data_runner_policy.py` pins
// that there is exactly one; two delete sites means two places to get the guards right.
// Throws on failure; the caller decides whether that is worth retrying.
function removeDisposableProfile() {
  if (!DATA_DIR_IS_OURS) return;
  const root = path.resolve(os.tmpdir());
  const target = path.resolve(DATA_DIR);
  if (target === root || !target.startsWith(root + path.sep)) {
    console.log(`==> Leaving ${target} in place (outside the temp root — refusing to remove it).`);
    return;
  }
  fs.rmSync(target, { recursive: true, force: true });
  console.log(`==> Removed the disposable profile ${target}`);
}

async function cleanupProfile() {
  const deadline = Date.now() + 15000;
  for (;;) {
    try {
      removeDisposableProfile();
      return;
    } catch (e) {
      if (Date.now() >= deadline) {
        // Non-fatal: a leaked temp directory must never turn a passing verification run into a failure.
        console.log(`==> Could not remove the disposable profile: ${e.message}`);
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
// A PRECONDITION failure happens before the app is ever spawned, so the profile it would have used is
// an EMPTY directory — the "keep it for the post-mortem" reasoning in cleanupProfile does not apply,
// because there is no DB in there to read. Measured 2026-08-01: two 0-file `cortex-e2e-*` directories
// from two precondition exits minutes earlier, among 40 totalling 865 MB. Nothing is spawned yet, so
// no handles are held and a plain sync remove is enough; anything unexpected is swallowed, since
// failing to tidy up must never change the exit code the caller is being told about.
function die(msg) {
  console.error('PRECONDITION FAILED: ' + msg);
  // Through the SHARED remover, never a second delete of its own: the guards live in one place.
  // No retry — nothing has been spawned, so no handle is held open to wait for.
  try { removeDisposableProfile(); } catch { /* tidying up must not mask the precondition above */ }
  process.exit(1);
}

if (!fs.existsSync(AUDIO)) {
  die(process.env.CORTEX_AUDIO
    ? 'CORTEX_AUDIO does not exist: ' + AUDIO
    : 'the committed fixture is missing: ' + AUDIO + ' (set CORTEX_AUDIO to a real audio file instead).');
}
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
      // Never 8737: this run must not fight the owner's own Couch Review for the port, and must
      // never be mistaken for it by a phone that still has the real link open.
      CORTEX_COUCH_PORT: String(COUCH_PORT),
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

  await reviewOverHttp(page, rawText);

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

/**
 * Couch Review, over real HTTP, end to end.
 *
 * WHY THIS IS HERE. Couch Review is how clips actually get reviewed — multi-reviewer tokens, DPAPI
 * at rest, leases, spot checks, an offline outbox and a 68 KB phone page — and every test it had
 * called `api_queue` / `api_decision` as Rust FUNCTIONS. Nothing started the real binary, bound a
 * real port, claimed a token over the wire, streamed audio bytes, submitted a decision, and read the
 * library back. The most reviewer-facing surface in the app was the one with no end-to-end coverage.
 *
 * It rides in this harness rather than a second one so it inherits what is already hard-won here:
 * the disposable profile with its refusal to touch the real %APPDATA% library, the WebView2
 * isolation, and the retrying cleanup. One app, one profile, one place that knows how to kill it.
 *
 * Everything below is asserted against the LIBRARY, never against a 200. A server can answer 200 and
 * persist nothing.
 */
async function reviewOverHttp(page, draftText) {
  console.log('==> Couch Review over real HTTP...');
  const base = `http://127.0.0.1:${COUCH_PORT}`;
  const REVIEWER = 'E2E Harness';
  // EVERY request here is bounded. Measured the hard way on run 2 of the 3x stability check: the
  // post-stop probe below is a fetch whose whole purpose is to find NOBODY listening, and a bare
  // fetch has no timeout — when the closed port dropped the SYN instead of refusing it, the run sat
  // there forever. The app had already logged "Couch Review stopped"; the harness simply never
  // returned. A gate that hangs is worse than one that fails: a failure gets reported.
  const http = (url, opts = {}) => fetch(url, { ...opts, signal: AbortSignal.timeout(20000) });
  // Read one row back through the app's OWN IPC, so every assertion below sees what the desktop would
  // show rather than a hand-rolled query that could happen to agree with a broken write.
  const segmentById = (pg, id) => pg.evaluate(
    (sid) => window.__TAURI_INTERNALS__.invoke('get_segments', { verified: null })
      .then((all) => all.find((s) => s.id === sid) || null),
    id,
  );

  const status = await page.evaluate(
    (r) => window.__TAURI_INTERNALS__.invoke('start_couch_review', { reviewers: [r] }),
    REVIEWER,
  );
  if (!status || !status.running || !status.reviewers || !status.reviewers.length) {
    throw new Error('start_couch_review did not return a running session: ' + JSON.stringify(status));
  }
  // The token rides in the URL FRAGMENT (#t=...) precisely so it never reaches a server in a request
  // line; the page POSTs it once to /api/claim and lives on the HttpOnly cookie after that. Drive the
  // same path a phone does — the ?t= legacy form would skip the half most reviewers actually use.
  const token = (status.reviewers[0].url.split('#t=')[1] || '').trim();
  if (!token) throw new Error('no token in the reviewer URL: ' + status.reviewers[0].url);
  // NEVER print the token: it is a live credential for the review server.
  console.log(`   session up for ${JSON.stringify(status.reviewers[0].name)} on port ${COUCH_PORT}`);

  const claim = await http(`${base}/api/claim`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ token }),
  });
  if (claim.status !== 200) throw new Error(`POST /api/claim -> ${claim.status}, expected 200`);
  const cookie = (claim.headers.getSetCookie() || []).map((c) => c.split(';')[0]).join('; ');
  if (!cookie) throw new Error('/api/claim returned no Set-Cookie — the phone would have nothing to ride on');

  const queueRes = await http(`${base}/api/queue`, { headers: { cookie } });
  if (queueRes.status !== 200) throw new Error(`GET /api/queue -> ${queueRes.status}, expected 200`);
  const queue = await queueRes.json();
  const item = (queue.items || [])[0];
  if (!item) throw new Error('the queue served no clips, so nothing here was actually reviewed');
  // The fields the page renders from. A missing one is a blank line on someone's phone.
  for (const key of ['id', 'text', 'durationMs', 'speakerChange']) {
    if (!(key in item)) throw new Error(`queue item is missing ${key}: ${JSON.stringify(item)}`);
  }
  if (queue.reviewer !== REVIEWER) {
    throw new Error(`queue says the reviewer is ${JSON.stringify(queue.reviewer)}, not ${JSON.stringify(REVIEWER)}`);
  }
  console.log(`   queue: ${queue.items.length} item(s), pendingTotal ${queue.pendingTotal}`);

  // Audio is the whole point of reviewing: a reviewer who cannot hear the clip cannot judge it.
  const audio = await http(`${base}/api/audio/${encodeURIComponent(item.id)}`, { headers: { cookie } });
  if (audio.status !== 200) throw new Error(`GET /api/audio -> ${audio.status}, expected 200`);
  const bytes = Buffer.from(await audio.arrayBuffer());
  if (bytes.length < 1024 || bytes.subarray(0, 4).toString('latin1') !== 'RIFF') {
    throw new Error(`/api/audio served ${bytes.length} bytes starting ${JSON.stringify(bytes.subarray(0, 4).toString('latin1'))} — not a WAV`);
  }
  console.log(`   audio: ${bytes.length} bytes, RIFF header present`);

  // An unauthenticated request must NOT be served. Checking this here rather than trusting the unit
  // test means the check runs against the real listener, headers and all.
  const naked = await http(`${base}/api/queue`);
  if (naked.status === 200) throw new Error('GET /api/queue with NO credential returned 200 — the token gate is open');

  // THE NO-VERDICT PATH (R4.4), over real HTTP against the real library. A reviewer who cannot judge a
  // clip says so, and the one thing that must be true is that it writes NOTHING — the whole point is to
  // keep a guess out of the corpus. Asserted through the app's own IPC, not the reply, because a reply
  // can say "ok" over a write that happened anyway.
  const preSkip = await segmentById(page, item.id);
  if (!preSkip) throw new Error(`segment ${item.id} is not in the library before the skip`);
  const skipped = await http(`${base}/api/decision`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', cookie },
    body: JSON.stringify({ id: item.id, action: 'skip' }),
  });
  if (skipped.status !== 200) {
    throw new Error(`POST /api/decision action=skip -> ${skipped.status} ${await skipped.text()}, expected 200`);
  }
  const postSkip = await segmentById(page, item.id);
  for (const field of ['humanDecision', 'verdict', 'verified', 'reviewedBy', 'rawTranscript', 'annotatedTranscript']) {
    if (JSON.stringify(postSkip[field]) !== JSON.stringify(preSkip[field])) {
      throw new Error(
        `a skip changed ${field}: ${JSON.stringify(preSkip[field])} -> ${JSON.stringify(postSkip[field])}. ` +
        'A skip is the ABSENCE of a verdict and must write nothing to the row.',
      );
    }
  }
  // ...and the clip leaves THIS reviewer's queue while staying pending for everyone, which is the other
  // half of the contract: without it the next refill hands it straight back and the button is a treadmill.
  const afterSkip = await (await http(`${base}/api/queue`, { headers: { cookie } })).json();
  if ((afterSkip.items || []).some((s) => s.id === item.id)) {
    throw new Error(`the skipped clip ${item.id} was served straight back to the reviewer who skipped it`);
  }
  if (afterSkip.skippedByYou !== 1) {
    throw new Error(`queue reports skippedByYou=${afterSkip.skippedByYou}, expected 1 — the page would claim "all reviewed"`);
  }
  if (afterSkip.pendingTotal !== queue.pendingTotal) {
    throw new Error(`skipping changed pendingTotal ${queue.pendingTotal} -> ${afterSkip.pendingTotal}; an unjudged clip is still pending work`);
  }
  console.log(`   skip: nothing written, clip out of this reviewer's queue, backlog still ${afterSkip.pendingTotal}`);

  // The SAME clip, decided for real. A skip must not poison a clip: it is the absence of a verdict,
  // not a refusal, so the reviewer who skipped it can still come back and judge it.
  const corrected = draftText + ' ✔';
  const decision = await http(`${base}/api/decision`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', cookie },
    body: JSON.stringify({ id: item.id, action: 'edit', text: corrected }),
  });
  if (decision.status !== 200) {
    throw new Error(`POST /api/decision -> ${decision.status} ${await decision.text()}, expected 200`);
  }

  // THE assertion: the library, not the response.
  const row = await segmentById(page, item.id);
  if (!row) throw new Error(`segment ${item.id} vanished from the library after the decision`);
  if (row.annotatedTranscript !== corrected) {
    throw new Error(`the correction did not persist: stored ${JSON.stringify(row.annotatedTranscript)}, sent ${JSON.stringify(corrected)}`);
  }
  if (!row.verified) throw new Error('a decided clip must leave the review queue (verified stayed false)');
  if (row.reviewedBy !== REVIEWER) {
    throw new Error(`decision attributed to ${JSON.stringify(row.reviewedBy)}, expected ${JSON.stringify(REVIEWER)}`);
  }
  console.log(`   decision persisted: verified=${row.verified}, reviewedBy=${JSON.stringify(row.reviewedBy)}`);

  await page.evaluate(() => window.__TAURI_INTERNALS__.invoke('stop_couch_review'));
  // Stopped means STOPPED — a listener left bound would outlive this run and hold the port.
  // A timeout counts as NOT answering, which is the outcome this asserts — so the abort above is
  // the difference between proving the port was released and hanging on the question.
  const afterStop = await http(`${base}/api/queue`, { headers: { cookie } }).then(() => true, () => false);
  if (afterStop) throw new Error('the server still answered after stop_couch_review');
  console.log('   server stopped and the port released');
}

run().catch((err) => {
  console.error('==> REAL-DATA RUN FAILED:', err && err.message ? err.message : err);
  killApp();
  // NOT cleaned up on failure — see cleanupProfile. Say where it is so the post-mortem can find it.
  console.error(`==> Profile kept for diagnosis: ${DATA_DIR}`);
  process.exit(1);
});
