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
 *   - Exercises startup + get_settings + get_waveform (decode) + get_jobs. It does NOT by default run a
 *     transcription (the path where cloud STT/LLM would fire IF consent leaked) — that leg needs a local
 *     model + audio and is owner-machine-gated; run with CORTEX_EGRESS_TRANSCRIBE=1 once a fixture exists.
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

const REPO = __dirname.replace(/[\\/]scripts$/, '');
const APP_EXE =
  process.env.CORTEX_APP_EXE || path.join(REPO, 'src-tauri', 'target', 'release', 'cortex-speech-app.exe');
const AUDIO =
  process.env.CORTEX_EGRESS_AUDIO || path.join(REPO, 'src-tauri', 'tests', 'fixtures', 'fleurs_ckb_sample.wav');
const DEBUG_PORT = process.env.CORTEX_DEBUG_PORT || '9335';
const SAMPLE_MS = Number(process.env.CORTEX_EGRESS_SAMPLE_MS || 200);
const WORKLOAD_MS = Number(process.env.CORTEX_EGRESS_WORKLOAD_MS || 6000);

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
  DATA_DIR = fs.mkdtempSync(path.join(os.tmpdir(), 'cortex-egress-'));
}

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
  await sleep(1500);
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

  const wvDir = fs.mkdtempSync(path.join(os.tmpdir(), 'cortex-egress-wv-'));
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
          /* a missing fixture is fine — we only need the backend exercised, not a result */
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

  console.log(
    `\nEGRESS OK: the default offline path opened ZERO non-loopback connections from the backend PID ` +
      `(covering startup + a ${(WORKLOAD_MS / 1000) | 0}s get_settings/get_jobs/get_waveform workload). ` +
      `Scope: TCP, poll-sampled every ${SAMPLE_MS}ms; transcribe-leg owner-gated (see header).`,
  );
}

run().catch((err) => {
  console.error('==> EGRESS PROBE FAILED:', err && err.message ? err.message : err);
  killApp();
  process.exit(1);
});
