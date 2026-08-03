#!/usr/bin/env node
/**
 * heartbeat_probe.cjs — RUNTIME proof for audit-point #2 ("every slow op keeps the UI responsive").
 *
 * Drives the REAL built exe and, while several slow async commands (get_waveform, which decodes a
 * clip) run concurrently, measures how long the instant get_settings command takes IN-PAGE. If the
 * slow commands were still sync (running on Tauri's main/UI thread), get_settings would stall behind
 * them; because they are now `async fn` (dispatched off the main thread), get_settings must stay
 * fast. This is the same repro shape that proved the Open/Import dialog-freeze fix.
 *
 * Isolation (same contract as e2e_real_app.cjs): runs against a DISPOSABLE CORTEX_APP_DATA_DIR,
 * REFUSES the real %APPDATA%\cortex-speech profile, and kills ONLY the process tree it spawned.
 *
 * Env:
 *   CORTEX_APP_EXE        (optional) path to cortex-speech-app.exe; default: repo release build
 *   CORTEX_APP_DATA_DIR   (optional) disposable profile dir; default: a fresh temp dir (prod refused)
 *   CORTEX_HEARTBEAT_AUDIO(optional) wav to decode for the slow load; default: committed CC-BY fixture
 *   CORTEX_HEARTBEAT_MAX_MS(optional) pass threshold for get_settings p95 latency; default 300
 *   CORTEX_DEBUG_PORT     (optional) WebView2 remote-debug port; default 9333
 *
 * Exit 0 iff get_settings p95 latency stays under the threshold while the slow commands run.
 */
const { spawn, execSync } = require('child_process');
const { chromium } = require('@playwright/test');
const path = require('path');
const fs = require('fs');
const os = require('os');

const REPO = __dirname.replace(/[\\/]scripts$/, '');
const APP_EXE =
  process.env.CORTEX_APP_EXE || path.join(REPO, 'src-tauri', 'target', 'release', 'cortex-speech-app.exe');
const AUDIO =
  process.env.CORTEX_HEARTBEAT_AUDIO || path.join(REPO, 'src-tauri', 'tests', 'fixtures', 'fleurs_ckb_sample.wav');
const DEBUG_PORT = process.env.CORTEX_DEBUG_PORT || '9333';
const MAX_MS = Number(process.env.CORTEX_HEARTBEAT_MAX_MS || 300);
const NUM_SLOW = 8; // concurrent get_waveform decodes to keep the async pool busy
const PROBE_MS = 4000; // how long to hammer get_settings while the slow load runs

const die = (m) => {
  console.error('PRECONDITION FAILED: ' + m);
  process.exit(1);
};

if (!fs.existsSync(APP_EXE)) die(`app exe not found: ${APP_EXE} (build it: cargo build --release ...)`);
if (!fs.existsSync(AUDIO)) die(`heartbeat audio not found: ${AUDIO}`);

// ── Profile isolation: never the production library. ──
const PROD = process.env.APPDATA ? path.join(process.env.APPDATA, 'cortex-speech') : null;
const norm = (p) => path.resolve(p).replace(/[\\/]+$/, '').toLowerCase();
let DATA_DIR = process.env.CORTEX_APP_DATA_DIR;
if (DATA_DIR) {
  if (PROD && (norm(DATA_DIR) === norm(PROD) || norm(DATA_DIR).startsWith(norm(PROD) + path.sep))) {
    die(`CORTEX_APP_DATA_DIR points at the REAL profile (${PROD}); use a disposable dir.`);
  }
} else {
  DATA_DIR = fs.mkdtempSync(path.join(os.tmpdir(), 'cortex-heartbeat-'));
}

let appProcess = null;
// Module scope so cleanupTemp() and the failure handler can both reach it; assigned in run().
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
  console.log(`==> Heartbeat probe. profile=${DATA_DIR}  audio=${path.basename(AUDIO)}  threshold=${MAX_MS}ms`);
  if (await fetch(`http://127.0.0.1:${DEBUG_PORT}/json`).then((r) => r.ok, () => false)) {
    die(`debug port ${DEBUG_PORT} already answering — another instance is running; close it or set CORTEX_DEBUG_PORT.`);
  }

  // Isolate the WebView2 user-data folder per run — sharing the default one across concurrent /
  // rapid launches (or with the owner's other WebView2 apps) yields WebView2 error 0x8007139F
  // ("resource not in the correct state") and the window never opens.
  wvDir = fs.mkdtempSync(path.join(os.tmpdir(), 'cortex-hb-wv-'));
  appProcess = spawn(APP_EXE, [], {
    env: {
      ...process.env,
      CORTEX_APP_DATA_DIR: DATA_DIR,
      WEBVIEW2_USER_DATA_FOLDER: wvDir,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${DEBUG_PORT}`,
    },
    cwd: path.dirname(APP_EXE),
    shell: false,
    // Discard the exe's stdio: the app logs verbosely (ort init) at startup, and an undrained pipe
    // buffer would fill and BLOCK the exe mid-startup so WebView2 never initializes.
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
    async ({ audio, numSlow, probeMs }) => {
      const invoke = window.__TAURI_INTERNALS__.invoke;
      // Warm up once (first decode may include one-time setup we don't want to count).
      try {
        await invoke('get_waveform', { path: audio, numPoints: 2000, alignmentJson: null });
      } catch (e) {
        /* ignore */
      }
      // Launch the slow async load and DON'T await it yet.
      const slow = [];
      for (let i = 0; i < numSlow; i++) {
        slow.push(invoke('get_waveform', { path: audio, numPoints: 6000, alignmentJson: null }).catch(() => {}));
      }
      // Hammer the instant get_settings command and record its latency while the slow load runs.
      const lat = [];
      const end = performance.now() + probeMs;
      while (performance.now() < end) {
        const t0 = performance.now();
        try {
          await invoke('get_settings');
        } catch (e) {
          /* ignore */
        }
        lat.push(performance.now() - t0);
      }
      await Promise.allSettled(slow);
      lat.sort((a, b) => a - b);
      const at = (q) => lat[Math.min(lat.length - 1, Math.floor(lat.length * q))];
      return { count: lat.length, median: at(0.5), p95: at(0.95), max: lat[lat.length - 1] };
    },
    { audio: AUDIO, numSlow: NUM_SLOW, probeMs: PROBE_MS },
  );

  await browser.close();
  killApp();

  console.log(
    `==> get_settings during ${NUM_SLOW} concurrent get_waveform decodes: ` +
      `${result.count} calls · median ${result.median.toFixed(1)}ms · p95 ${result.p95.toFixed(1)}ms · max ${result.max.toFixed(1)}ms`,
  );
  if (result.p95 > MAX_MS) {
    throw new Error(
      `MAIN THREAD NOT RESPONSIVE: get_settings p95 ${result.p95.toFixed(1)}ms exceeds ${MAX_MS}ms while slow commands ran.`,
    );
  }
  console.log(`\nHEARTBEAT OK: main thread stayed responsive (p95 ${result.p95.toFixed(1)}ms <= ${MAX_MS}ms).`);
  // SETTLE before removing the profile. `taskkill /F /T` returns before Windows has released the
  // killed tree's file handles, and the app holds SQLite's .db/-wal/-shm open. Measured: without this
  // the WebView2 folder deleted fine but the data profile failed EPERM every time, so the leak was
  // only half fixed. rmSync's own maxRetries does not help — it retries the CALL, not the wait for a
  // handle that is still open when the first attempt is made.
  await sleep(1500);
  cleanupTemp();
}

// Remove the two temp trees this probe creates, but ONLY on success — the same contract the header
// claims to share with e2e_real_app.cjs, which was never actually implemented here. Measured
// 2026-08-03: 29 `cortex-heartbeat-*` profiles and 24 `cortex-hb-wv-*` WebView2 folders left behind,
// 168 MB, one pair per sweep since the probe was written.
//
// Kept on FAILURE on purpose, and the path is printed: a failed run is the one whose profile someone
// needs to open. That is e2e_real_app.cjs's rule too.
function cleanupTemp() {
  for (const dir of [DATA_DIR, wvDir]) {
    // WebView2 can hold files for a moment after the app dies; a leftover directory is untidy, never
    // a reason to fail a green run.
    try {
      if (dir) fs.rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
    } catch (e) {
      console.log(`   (could not remove ${dir}: ${e.message})`);
    }
  }
}

run().catch((err) => {
  console.error('==> HEARTBEAT FAILED:', err && err.message ? err.message : err);
  killApp();
  console.error(`==> Profiles kept for diagnosis: ${DATA_DIR}${wvDir ? ` and ${wvDir}` : ''}`);
  process.exit(1);
});
