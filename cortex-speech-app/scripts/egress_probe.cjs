#!/usr/bin/env node
/**
 * egress_probe.cjs — RUNTIME proof for audit-point #9 ("default runtime egress measured as zero").
 *
 * Launches the REAL built exe with DEFAULT (fully-offline) settings and, while it starts up and serves
 * a representative offline workflow, monitors the MAIN exe process's outbound TCP connections. It asserts
 * ZERO connections to any non-loopback / non-local address — i.e. the default path opens no connection to
 * the public internet (no telemetry, no update check, no cloud STT/LLM).
 *
 * Why the MAIN exe PID only: cloud requests originate in the Rust backend (reqwest/ureq) inside the main
 * `cortex-speech-app.exe` process. The WebView2 child processes (msedgewebview2.exe, separate PIDs) have
 * their own benign runtime traffic (component updates, etc.) that is NOT the app's egress — monitoring the
 * backend PID isolates the property the audit cares about.
 *
 * HONEST SCOPE / LIMITATIONS (do not overclaim):
 *   - Measures the MAIN process's TCP connections by polling (default 200ms). A connection that opens and
 *     closes entirely between two samples could be missed; a real cloud HTTPS call (DNS + TCP + TLS +
 *     request) lasts well over one sample, so a SUSTAINED phone-home is caught. This is a strong runtime
 *     signal, not a formal proof — a kernel/ETW socket trace would be the airtight version.
 *   - A Tauri backend legitimately owns ~0 TCP connections offline, so a zero result could otherwise mean
 *     "no egress" OR "the sampler silently never ran". A POSITIVE CONTROL (validateSamplerApparatus) runs
 *     first each execution: it opens a known loopback connection owned by this node process and requires
 *     the SAME sampler invocation to observe it, so a broken monitor fails LOUD instead of vacuously green.
 *   - Exercises startup + get_settings + get_waveform (decode) + get_jobs AND, when the offline CTC model
 *     is resolvable, a REAL transcription (import -> VAD -> CTC ASR -> persist) — the path where cloud
 *     STT/LLM would fire IF consent leaked. The transcribe leg auto-runs when the local model is present;
 *     force with CORTEX_EGRESS_TRANSCRIBE=1, skip with =0. If enabled but ASR yields 0 segments the leg is
 *     reported INCONCLUSIVE (not silently claimed) — the egress verdict then covers startup+browse only.
 *   - The airtight non-poll version (a kernel/ETW socket trace) is still a further stretch; this poll-based
 *     probe with a positive control is a strong runtime signal, not a formal proof.
 *   - TCP only. A UDP/DNS lookup with no TCP connect is not counted (no data egress without a connection).
 *
 * Isolation (same contract as heartbeat_probe.cjs / jobs_probe.cjs): a DISPOSABLE CORTEX_APP_DATA_DIR, a
 * per-run WEBVIEW2_USER_DATA_FOLDER, REFUSES the real %APPDATA%\cortex-speech profile, kills ONLY the
 * process tree it spawned. Exit 0 iff no external connection is observed from the main exe PID.
 *
 * Env: CORTEX_APP_EXE (default: release build), CORTEX_DEBUG_PORT (default 9335),
 *      CORTEX_EGRESS_SAMPLE_MS (default 200), CORTEX_EGRESS_WORKLOAD_MS (default 6000).
 */
const { spawn, execSync } = require('child_process');
const { chromium } = require('@playwright/test');
const net = require('net');
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
const AUDIO =
  process.env.CORTEX_EGRESS_AUDIO || path.join(REPO, 'src-tauri', 'tests', 'fixtures', 'fleurs_ckb_sample.wav');
const DEBUG_PORT = process.env.CORTEX_DEBUG_PORT || '9335';
const SAMPLE_MS = Number(process.env.CORTEX_EGRESS_SAMPLE_MS || 200);
const WORKLOAD_MS = Number(process.env.CORTEX_EGRESS_WORKLOAD_MS || 6000);

// Transcribe leg: run a REAL offline ASR under the monitor when the CTC-300M model is resolvable.
// Detection MUST match the APP's own resolver (models.rs bundled/repo fallback), which finds the model
// at EITHER target/release/models (next to the exe) OR src-tauri/models (the fetch_models.py
// destination that every other model gate checks). Detecting only next-to-exe silently skipped the leg
// on the canonical fetch-models layout while the app could still transcribe — a false "browse-only"
// pass under a gate whose charter claims ASR coverage (adversarial review 2026-07-12). Auto-on when
// present; CORTEX_EGRESS_TRANSCRIBE forces (1) or skips (0).
const CTC_REL = path.join('omniasr-ctc-300m', 'model.int8.onnx');
const CTC_CANDIDATES = [
  path.join(path.dirname(APP_EXE), 'models', CTC_REL),
  path.join(REPO, 'src-tauri', 'models', CTC_REL),
];
const CTC_PRESENT = CTC_CANDIDATES.some((p) => fs.existsSync(p));
const TRANSCRIBE_ENV = process.env.CORTEX_EGRESS_TRANSCRIBE;
const RUN_TRANSCRIBE = TRANSCRIBE_ENV === '1' || (TRANSCRIBE_ENV !== '0' && CTC_PRESENT);

// Temp dirs WE created, registered as they are made. `die()` fires on a PRECONDITION failure — before
// the app is ever launched — so these are empty and there is nothing in them to diagnose. That is the
// opposite of the failure handler at the bottom, which deliberately KEEPS profiles for inspection.
// Declared above `die` so it can reach the list without a temporal-dead-zone read of the consts below.
const ownedTemp = [];
const die = (m) => {
  console.error('PRECONDITION FAILED: ' + m);
  for (const d of ownedTemp) {
    try {
      fs.rmSync(d, { recursive: true, force: true });
    } catch (e) {
      /* best effort: the exit code is what matters here */
    }
  }
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
  DATA_DIR = fs.mkdtempSync(path.join(os.tmpdir(), 'cortex-egress-'));
  ownedTemp.push(DATA_DIR);
}

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

// A connection is "local" (not egress) if it targets loopback, the unspecified address, or link-local.
function isLocalAddress(addr) {
  if (!addr) return true;
  const a = String(addr).trim().toLowerCase();
  return (
    a === '127.0.0.1' ||
    a.startsWith('127.') ||
    a === '::1' ||
    a === '0.0.0.0' ||
    a === '::' ||
    a === '' ||
    a.startsWith('169.254.') || // IPv4 link-local
    a.startsWith('fe80') // IPv6 link-local
  );
}

// Start a background PowerShell that continuously samples the target PID's TCP connections and STREAMS
// each newly-seen `RemoteAddress|RemotePort|State` line as it appears. Runs concurrently with the CDP
// workload (spawn is non-blocking, unlike execSync). Returns a handle; call stopSampler() to end it and
// collect the distinct endpoints observed.
function startSampler(pid) {
  const ps =
    `$ErrorActionPreference='SilentlyContinue';$seen=@{};` +
    `while($true){Get-NetTCPConnection -OwningProcess ${pid} 2>$null|ForEach-Object{` +
    `$k="$($_.RemoteAddress)|$($_.RemotePort)|$($_.State)";` +
    `if(-not $seen.ContainsKey($k)){$seen[$k]=$true;Write-Output $k}};` +
    `Start-Sleep -Milliseconds ${SAMPLE_MS}}`;
  const proc = spawn('powershell', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-Command', ps], {
    stdio: ['ignore', 'pipe', 'ignore'],
  });
  const lines = [];
  let buf = '';
  proc.stdout.on('data', (chunk) => {
    buf += chunk.toString();
    let nl;
    while ((nl = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, nl).trim();
      buf = buf.slice(nl + 1);
      if (line) lines.push(line);
    }
  });
  return { proc, lines };
}

function stopSampler(handle) {
  try {
    if (handle.proc && handle.proc.pid) execSync(`taskkill /F /T /PID ${handle.proc.pid}`, { stdio: 'ignore' });
  } catch (e) {
    /* already gone */
  }
  const all = handle.lines
    .map((l) => l.split('|'))
    .filter((p) => p.length >= 3)
    .map(([addr, port, state]) => ({ addr, port, state, raw: `${addr}:${port} [${state}]` }));
  return { external: all.filter((c) => !isLocalAddress(c.addr)), total: all.length };
}

// POSITIVE CONTROL (anti-vacuous-pass): a Tauri backend legitimately owns ~0 TCP connections on the
// offline path, so `total == 0` is BOTH the healthy result AND the signature of a silently-dead sampler.
// Before trusting any zero, prove the sampler apparatus actually works THIS run, with the SAME spawn
// invocation: open a known loopback TCP connection owned by THIS node process and require the sampler to
// observe it. If it sees nothing, the monitor is broken -> fail LOUD instead of a vacuous "EGRESS OK".
async function validateSamplerApparatus() {
  const server = net.createServer(() => {});
  await new Promise((res, rej) => {
    server.once('error', rej);
    server.listen(0, '127.0.0.1', res);
  });
  const port = server.address().port;
  const client = net.connect(port, '127.0.0.1');
  await new Promise((res, rej) => {
    client.once('connect', res);
    client.once('error', rej);
  });
  const control = startSampler(process.pid); // this node process owns the loopback socket above
  // Poll until the sampler observes the known socket, up to 4s — a fixed short window can be shorter
  // than PowerShell/Get-NetTCPConnection cold start, which would FALSELY fail the control (a flaky
  // gate). This passes the instant the apparatus works, and only fails after a genuine 4s of nothing.
  for (let i = 0; i < 20 && control.lines.length === 0; i++) await sleep(200);
  const seen = stopSampler(control);
  client.destroy();
  server.close();
  if (seen.total < 1) {
    throw new Error(
      'POSITIVE CONTROL FAILED: the sampler observed 0 TCP connections for a PID (this node process) that ' +
        'has a known open loopback socket. The monitor is not functioning (PowerShell/Get-NetTCPConnection ' +
        'unavailable, blocked, or mis-invoked), so a zero-egress result would be vacuous — refusing to pass.',
    );
  }
  console.log(`==> positive control OK: sampler saw ${seen.total} connection(s) for the control PID (apparatus works)`);
}

async function run() {
  console.log(`==> Egress probe. profile=${DATA_DIR}  debugPort=${DEBUG_PORT}  sample=${SAMPLE_MS}ms`);
  // Prove the monitor works BEFORE trusting any zero — a dead sampler must fail here, not pass as "no egress".
  await validateSamplerApparatus();
  if (await fetch(`http://127.0.0.1:${DEBUG_PORT}/json`).then((r) => r.ok, () => false)) {
    die(`debug port ${DEBUG_PORT} already answering — another instance is running; set CORTEX_DEBUG_PORT.`);
  }

  wvDir = fs.mkdtempSync(path.join(os.tmpdir(), 'cortex-egress-wv-'));
  ownedTemp.push(wvDir);
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
  const mainPid = appProcess.pid;
  console.log(`==> spawned main exe pid=${mainPid}`);

  // Begin sampling IMMEDIATELY so STARTUP egress (a telemetry/update phone-home would hide here) is
  // covered from t0 — the sampler streams concurrently through the debug-port wait and the CDP workload.
  const sampler = startSampler(mainPid);

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

  // Transcribe leg (highest-value) runs FIRST — before the browse busy-loop below, which hammers
  // get_settings, trips the backend's IPC rate limiter and would otherwise reject this leg's own
  // get_settings and make it inconclusive. Run a REAL offline transcription while the SAME sampler
  // (started at t0) watches, so any cloud STT/LLM connection on the ASR path would be caught. The CTC
  // engine is provisioned in the disposable profile (default WSL7B fail-hards without its server);
  // cloud opt-ins are asserted OFF below, so this stays the offline path.
  let transcribeLeg = { ran: false, segments: 0, error: null };
  if (RUN_TRANSCRIBE) {
    console.log('==> transcribe leg: provisioning offline CTC engine + importing real audio under the monitor...');
    transcribeLeg = await page.evaluate(
      async ({ audio }) => {
        const invoke = window.__TAURI_INTERNALS__.invoke;
        try {
          const s = await invoke('get_settings');
          s.asr_model_size = 'CTC300M';
          s.use_finetuned_asr = false;
          // CPU CTC: egress is identical either way, and it can't contend for VRAM a warm 7B server
          // may be holding on both cards — keeps the leg robust, not fast (speed is not this test).
          s.enable_gpu = false;
          await invoke('update_settings', { settings: s });
          await invoke('import_audio_file', { path: audio });
          // Poll the backend for segments — proof the ASR path actually ran (up to 120s).
          let segs = [];
          for (let i = 0; i < 120; i++) {
            segs = await invoke('get_segments', { verified: null }).catch(() => []);
            if (Array.isArray(segs) && segs.length >= 1) break;
            await new Promise((r) => setTimeout(r, 1000));
          }
          return { ran: true, segments: Array.isArray(segs) ? segs.length : 0, error: null };
        } catch (e) {
          return { ran: true, segments: 0, error: String((e && e.message) || e) };
        }
      },
      { audio: AUDIO },
    );
  }

  const workload = await page.evaluate(
    async ({ audio, workloadMs }) => {
      const invoke = window.__TAURI_INTERNALS__.invoke;
      const settings = await invoke('get_settings');
      // Exercise the default-offline workflow for the window while the sampler watches.
      const end = performance.now() + workloadMs;
      let calls = 0;
      while (performance.now() < end) {
        try {
          await invoke('get_settings');
          await invoke('get_jobs');
          await invoke('get_waveform', { path: audio, numPoints: 4000, alignmentJson: null });
        } catch (e) {
          /* a missing fixture / rate-limit is fine — we only need the backend exercised, not a result */
        }
        calls++;
      }
      return {
        cloud_llm_opt_in: !!settings.cloud_llm_opt_in,
        cloud_stt_opt_in: !!settings.cloud_stt_opt_in,
        jury_cloud_opt_in: !!settings.jury_cloud_opt_in,
        calls,
      };
    },
    { audio: AUDIO, workloadMs: WORKLOAD_MS },
  );

  await browser.close();

  // Stop sampling now that the workload is done, and collect what the backend PID connected to.
  const result = stopSampler(sampler);
  killApp();

  // Precondition: this must be the genuine DEFAULT offline path (no cloud opted in), else a "zero egress"
  // pass would be meaningless.
  if (workload.cloud_llm_opt_in || workload.cloud_stt_opt_in || workload.jury_cloud_opt_in) {
    throw new Error(
      `NOT the default-offline path — a cloud opt-in is ON (llm=${workload.cloud_llm_opt_in} ` +
        `stt=${workload.cloud_stt_opt_in} jury=${workload.jury_cloud_opt_in}); the egress claim would be void.`,
    );
  }

  console.log(
    `==> default-offline workflow: ${workload.calls} loops · cloud opt-ins OFF · ` +
      `sampled ${result.total} distinct backend TCP endpoints (loopback + external)`,
  );

  if (result.external.length > 0) {
    console.error('==> EXTERNAL connections observed from the main exe PID:');
    for (const c of result.external) console.error(`    ${c.raw}`);
    throw new Error(
      `EGRESS DETECTED: the default offline path opened ${result.external.length} non-local connection(s).`,
    );
  }

  // If we DECIDED to run the transcribe leg (model present / forced) it MUST have proven the ASR path
  // (>=1 segment). A 0-segment leg cannot back the ASR-coverage the gate advertises, so fail LOUD
  // rather than pass claiming coverage we did not get — the whole reason this leg exists is to monitor
  // the import->VAD->CTC path where a cloud-STT/LLM leak would fire. (Egress itself was clean for what
  // ran; this is a leg-inconclusive failure, not an egress failure.)
  if (transcribeLeg.ran && transcribeLeg.segments < 1) {
    throw new Error(
      `TRANSCRIBE LEG INCONCLUSIVE: the leg ran but produced 0 segments` +
        `${transcribeLeg.error ? ` (err=${transcribeLeg.error})` : ''} — cannot prove the ASR path was ` +
        `exercised, so refusing to pass a gate that advertises ASR-path egress coverage. ` +
        `(Zero external connections were observed for what ran; re-run, or set CORTEX_EGRESS_TRANSCRIBE=0 ` +
        `to intentionally cover startup+browse only.)`,
    );
  }
  // Only two honest outcomes reach here: the leg ran AND proved ASR (segments>=1), or it was not run
  // (no model / explicitly skipped → startup+browse only). Never a silent "ran but empty" pass.
  const legDesc = transcribeLeg.ran
    ? `+ a REAL offline transcription (${transcribeLeg.segments} segment(s) via CTC ASR)`
    : `; transcribe leg skipped (no local CTC model) — verdict covers startup+browse only`;
  console.log(
    `\nEGRESS OK: the default offline path opened ZERO non-loopback connections from the backend PID ` +
      `(covering startup + a ${(WORKLOAD_MS / 1000) | 0}s get_settings/get_jobs/get_waveform workload ${legDesc}). ` +
      `Scope: TCP, poll-sampled every ${SAMPLE_MS}ms; airtight kernel/ETW trace still a stretch (see header).`,
  );
  // Cleaned on SUCCESS only: a failed run is the one whose profile someone needs to open, and the
  // failure handler prints where it is. Same rule as e2e_real_app.cjs. OWNS_DATA_DIR guards the case
  // where the caller supplied their own CORTEX_APP_DATA_DIR -- that directory is not ours to delete.
  await cleanupProfile(DATA_DIR, OWNS_DATA_DIR);
  await cleanupProfile(wvDir, true);
}

run().catch((err) => {
  console.error('==> EGRESS PROBE FAILED:', err && err.message ? err.message : err);
  console.error(`==> Profiles kept for diagnosis: ${DATA_DIR}${wvDir ? ` and ${wvDir}` : ''}`);
  killApp();
  process.exit(1);
});
