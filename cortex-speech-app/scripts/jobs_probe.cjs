#!/usr/bin/env node
/**
 * jobs_probe.cjs — RUNTIME proof for audit-point #3 (durable Job Supervisor).
 *
 * Drives the REAL built exe: runs a real `export_dataset` command, then reads `get_jobs` and asserts a
 * durable `export_dataset` job was recorded and reached `succeeded`. This proves the run_tracked
 * bracketing + get_jobs surface end-to-end at runtime, not just in unit tests — the same class of proof
 * heartbeat_probe.cjs gives for main-thread safety.
 *
 * Isolation (same contract as heartbeat_probe.cjs / e2e_real_app.cjs): a DISPOSABLE CORTEX_APP_DATA_DIR,
 * a per-run WEBVIEW2_USER_DATA_FOLDER, REFUSES the real %APPDATA%\cortex-speech profile, kills ONLY the
 * process tree it spawned. Exit 0 iff the export job is present and succeeded.
 *
 * Env: CORTEX_APP_EXE (default: repo release build), CORTEX_DEBUG_PORT (default 9334).
 */
const { spawn, execSync } = require('child_process');
const { chromium } = require('@playwright/test');
const path = require('path');
const fs = require('fs');
const os = require('os');
// The SHARED disposable-profile cleanup, not a local copy. It refuses to delete anything outside
// tmpdir, retries to a 15s deadline because Windows releases the killed tree's handles
// asynchronously, and is non-fatal. Its own comment records the measurement that produced it: 34
// stale profiles totalling 764 MB left by e2e_real_app.
const { cleanupProfile } = require('../e2e_profile.cjs');

const REPO = __dirname.replace(/[\\/]scripts$/, '');
const APP_EXE =
  process.env.CORTEX_APP_EXE || path.join(REPO, 'src-tauri', 'target', 'release', 'cortex-speech-app.exe');
const DEBUG_PORT = process.env.CORTEX_DEBUG_PORT || '9334';

const die = (m) => {
  console.error('PRECONDITION FAILED: ' + m);
  process.exit(1);
};
if (!fs.existsSync(APP_EXE)) die(`app exe not found: ${APP_EXE} (build it: cargo build --release ...)`);

// ── Profile isolation: never the production library. ──
const PROD = process.env.APPDATA ? path.join(process.env.APPDATA, 'cortex-speech') : null;
const norm = (p) => path.resolve(p).replace(/[\\/]+$/, '').toLowerCase();
// Captured BEFORE the mkdtemp below: afterwards DATA_DIR is set either way and the distinction is
// unrecoverable. Did WE create this directory, or did the caller hand it to us? Only the first may be
// deleted -- removing a caller's own CORTEX_APP_DATA_DIR to tidy up after ourselves would be
// destroying their data.
const OWNS_DATA_DIR = !process.env.CORTEX_APP_DATA_DIR;
let DATA_DIR = process.env.CORTEX_APP_DATA_DIR;
if (DATA_DIR) {
  if (PROD && (norm(DATA_DIR) === norm(PROD) || norm(DATA_DIR).startsWith(norm(PROD) + path.sep))) {
    die(`CORTEX_APP_DATA_DIR points at the REAL profile (${PROD}); use a disposable dir.`);
  }
} else {
  DATA_DIR = fs.mkdtempSync(path.join(os.tmpdir(), 'cortex-jobs-'));
}
const OUT = path.join(DATA_DIR, 'jobs_probe_export.jsonl');
const HF_OUT = path.join(DATA_DIR, 'jobs_probe_hf'); // HF export writes a directory (parent must exist)

let appProcess = null;
// Module scope so the cleanup and the failure handler can both reach it; assigned in the run.
let wvDir = null;
function killApp() {
  if (appProcess && appProcess.pid) {
    try {
      execSync(`taskkill /F /T /PID ${appProcess.pid}`, { stdio: 'ignore' });
    } catch (e) {
      /* already gone */
    }
    appProcess = null;
  }
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function run() {
  console.log(`==> Jobs probe. profile=${DATA_DIR}  out=${path.basename(OUT)}`);
  if (await fetch(`http://127.0.0.1:${DEBUG_PORT}/json`).then((r) => r.ok, () => false)) {
    die(`debug port ${DEBUG_PORT} already answering — another instance is running; set CORTEX_DEBUG_PORT.`);
  }

  wvDir = fs.mkdtempSync(path.join(os.tmpdir(), 'cortex-jobs-wv-'));
  appProcess = spawn(APP_EXE, [], {
    env: {
      ...process.env,
      CORTEX_APP_DATA_DIR: DATA_DIR,
      WEBVIEW2_USER_DATA_FOLDER: wvDir,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${DEBUG_PORT}`,
    },
    cwd: path.dirname(APP_EXE),
    shell: false,
    stdio: 'ignore',
  });

  let pages = null;
  for (let i = 0; i < 90; i++) {
    try {
      const res = await fetch(`http://127.0.0.1:${DEBUG_PORT}/json`);
      if (res.ok) {
        pages = await res.json();
        break;
      }
    } catch (e) {
      /* not up yet */
    }
    await sleep(1000);
  }
  if (!pages) throw new Error(`WebView2 debug port ${DEBUG_PORT} did not come up within 90s.`);

  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${DEBUG_PORT}`);
  const ctx = browser.contexts()[0];
  const page = ctx.pages().find((p) => p.url().includes('localhost') || p.url().includes('1420')) || ctx.pages()[0];
  await page.waitForSelector('[data-testid="app-root"]', { timeout: 45000 });

  const result = await page.evaluate(
    async ({ out, hfDir }) => {
      const invoke = window.__TAURI_INTERNALS__.invoke;
      // No jobs before we run anything.
      const before = await invoke('get_jobs');
      // Run REAL exports (an empty library exports an empty dataset — still a completed op = a job row).
      const errors = {};
      try {
        await invoke('export_dataset', { path: out, format: 'jsonl' });
      } catch (e) {
        errors.export_dataset = String(e);
      }
      try {
        await invoke('export_huggingface_dataset', { path: hfDir });
      } catch (e) {
        errors.export_huggingface_dataset = String(e);
      }
      const after = await invoke('get_jobs');
      return { before, after, errors };
    },
    { out: OUT, hfDir: HF_OUT },
  );

  await browser.close();
  killApp();

  const { before, after, errors } = result;
  console.log(`==> get_jobs before=${before.length} after=${after.length}  errors=${JSON.stringify(errors)}`);

  // Assert BOTH bracketed ops recorded a durable succeeded job of their kind.
  for (const kind of ['export_dataset', 'export_huggingface_dataset']) {
    if (errors[kind]) throw new Error(`${kind} itself failed: ${errors[kind]}`);
    const job = after.find((j) => j.kind === kind);
    if (!job) throw new Error(`no ${kind} job was recorded (get_jobs returned ${JSON.stringify(after)})`);
    console.log(`==> recorded job: id=${job.id} kind=${job.kind} state=${job.state} errorCode=${job.errorCode}`);
    if (job.state !== 'succeeded') {
      throw new Error(`${kind} job did not reach 'succeeded' (state=${job.state}, errorCode=${job.errorCode})`);
    }
  }
  console.log(`\nJOBS OK: durable export_dataset + export_huggingface_dataset jobs recorded and 'succeeded' at runtime.`);
  // Cleaned on SUCCESS only: a failed run is the one whose profile someone needs to open, and the
  // failure handler prints where it is. Same rule as e2e_real_app.cjs. OWNS_DATA_DIR guards the case
  // where the caller supplied their own CORTEX_APP_DATA_DIR -- that directory is not ours to delete.
  await cleanupProfile(DATA_DIR, OWNS_DATA_DIR);
  await cleanupProfile(wvDir, true);
}

run().catch((err) => {
  console.error('==> JOBS PROBE FAILED:', err && err.message ? err.message : err);
  console.error(`==> Profiles kept for diagnosis: ${DATA_DIR}${wvDir ? ` and ${wvDir}` : ''}`);
  killApp();
  process.exit(1);
});
