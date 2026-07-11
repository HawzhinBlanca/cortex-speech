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
let DATA_DIR = process.env.CORTEX_APP_DATA_DIR;
if (DATA_DIR) {
  if (PROD && (norm(DATA_DIR) === norm(PROD) || norm(DATA_DIR).startsWith(norm(PROD) + path.sep))) {
    die(`CORTEX_APP_DATA_DIR points at the REAL profile (${PROD}); use a disposable dir.`);
  }
} else {
  DATA_DIR = fs.mkdtempSync(path.join(os.tmpdir(), 'cortex-jobs-'));
}
const OUT = path.join(DATA_DIR, 'jobs_probe_export.jsonl');

let appProcess = null;
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

  const wvDir = fs.mkdtempSync(path.join(os.tmpdir(), 'cortex-jobs-wv-'));
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
    async ({ out }) => {
      const invoke = window.__TAURI_INTERNALS__.invoke;
      // No jobs before we run anything.
      const before = await invoke('get_jobs');
      // Run a REAL export (an empty library exports an empty dataset — still a completed op = a job row).
      let exportError = null;
      try {
        await invoke('export_dataset', { path: out, format: 'jsonl' });
      } catch (e) {
        exportError = String(e);
      }
      const after = await invoke('get_jobs');
      return { before, after, exportError };
    },
    { out: OUT },
  );

  await browser.close();
  killApp();

  const { before, after, exportError } = result;
  console.log(`==> get_jobs before=${before.length} after=${after.length}  exportError=${exportError || 'none'}`);
  if (exportError) throw new Error(`export_dataset itself failed: ${exportError}`);

  const job = after.find((j) => j.kind === 'export_dataset');
  if (!job) throw new Error(`no export_dataset job was recorded (get_jobs returned ${JSON.stringify(after)})`);
  console.log(`==> recorded job: id=${job.id} kind=${job.kind} state=${job.state} errorCode=${job.errorCode}`);
  if (job.state !== 'succeeded') {
    throw new Error(`export job did not reach 'succeeded' (state=${job.state}, errorCode=${job.errorCode})`);
  }
  console.log(`\nJOBS OK: a durable export_dataset job was recorded and reached 'succeeded' at runtime.`);
}

run().catch((err) => {
  console.error('==> JOBS PROBE FAILED:', err && err.message ? err.message : err);
  killApp();
  process.exit(1);
});
