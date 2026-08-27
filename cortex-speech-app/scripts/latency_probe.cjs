#!/usr/bin/env node
/**
 * latency_probe.cjs — p50/p95 wall-clock for the operations a reviewer actually waits on.
 *
 * Deep audit P2 #10 asks for published reliability/performance evidence: "p50/p95 import, first
 * transcript, search, metadata-save, and export on long files". Durable review-decision latency is
 * a separate evidence leg because it requires real playback authority; this probe must not relabel a
 * metadata edit as a human decision. The repo already measures RTF
 * (`rtf-bench`), twelve microbenchmarks (`bench-budget`) and durability under hard kill
 * (`durability-drill`) — none of which is the number a human experiences. This measures that number,
 * end to end, through the REAL IPC on the REAL exe.
 *
 * WHY IN-PAGE TIMING. Each sample brackets a single `invoke(...)` inside the webview, so it includes
 * the IPC hop, the backend work and the reply — exactly the wait a reviewer sees. Timing from Node
 * would add process-spawn and CDP round-trip noise that no user ever pays.
 *
 * PERCENTILES, NOT A MEAN. A mean hides the tail, and the tail is what makes an app feel broken. p95 is
 * reported alongside p50 so a bimodal operation (cache hit vs miss) cannot masquerade as a fast one.
 *
 * NOT A GATE. It prints and records; nothing here fails a sweep. An end-to-end latency budget on a
 * developer desktop would red on background load rather than on regressions — the exact cry-wolf
 * failure that `bench-budget`'s confirm-re-measure exists to avoid — and a gate people learn to re-run
 * is worse than no gate. Publish the evidence; gate the microbenchmarks that are actually stable.
 *
 * Isolation is the same contract as every sibling probe: a DISPOSABLE profile, refusal of the real
 * library, its own debug port, and only the process tree it spawned is killed.
 *
 * Env: CORTEX_APP_EXE, CORTEX_DEBUG_PORT (default 9355), CORTEX_LATENCY_SAMPLES (default 12),
 *      CORTEX_AUDIO (the clip to import; default the committed CC-BY fixture),
 *      CORTEX_LATENCY_PACE_MS (default 250; see "PACE BETWEEN SAMPLES" below),
 *      CORTEX_ASR_ENGINE (optional explicit diagnostic override; default WSL7B).
 */
const { spawn, execSync } = require('child_process');
const { chromium } = require('@playwright/test');
const path = require('path');
const fs = require('fs');
const os = require('os');
const { cleanupProfile, provisionEngine } = require('../e2e_profile.cjs');

const REPO = __dirname.replace(/[\\/]scripts$/, '');
const APP_EXE =
  process.env.CORTEX_APP_EXE ||
  path.join(REPO, 'src-tauri', 'target', 'release', 'cortex-speech-app.exe');
const AUDIO =
  process.env.CORTEX_AUDIO ||
  path.join(REPO, 'src-tauri', 'tests', 'fixtures', 'fleurs_ckb_sample.wav');
const DEBUG_PORT = process.env.CORTEX_DEBUG_PORT || '9355';
const SAMPLES = Number(process.env.CORTEX_LATENCY_SAMPLES || 12);
const PACE_MS = Number(process.env.CORTEX_LATENCY_PACE_MS || 250);

const ownedTemp = [];
const die = (m) => {
  console.error('PRECONDITION FAILED: ' + m);
  for (const d of ownedTemp) {
    try {
      fs.rmSync(d, { recursive: true, force: true });
    } catch (e) {
      /* best effort */
    }
  }
  process.exit(1);
};
if (!fs.existsSync(APP_EXE)) die(`app exe not found: ${APP_EXE}`);
if (!fs.existsSync(AUDIO)) die(`audio fixture not found: ${AUDIO}`);

const PROD = process.env.APPDATA ? path.join(process.env.APPDATA, 'cortex-speech') : null;
const norm = (p) =>
  path
    .resolve(p)
    .replace(/[\\/]+$/, '')
    .toLowerCase();
const OWNS_DATA_DIR = !process.env.CORTEX_APP_DATA_DIR;
let DATA_DIR = process.env.CORTEX_APP_DATA_DIR;
if (DATA_DIR) {
  if (PROD && (norm(DATA_DIR) === norm(PROD) || norm(DATA_DIR).startsWith(norm(PROD) + path.sep))) {
    die(`CORTEX_APP_DATA_DIR points at the REAL profile (${PROD}); use a disposable dir.`);
  }
} else {
  DATA_DIR = fs.mkdtempSync(path.join(os.tmpdir(), 'cortex-latency-'));
  ownedTemp.push(DATA_DIR);
}

let appProcess = null;
let wvDir = null;
const killApp = () => {
  if (appProcess && appProcess.pid) {
    try {
      execSync(`taskkill /F /T /PID ${appProcess.pid}`, { stdio: 'ignore' });
    } catch (e) {
      /* already gone */
    }
    appProcess = null;
  }
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function stats(samples) {
  const s = [...samples].sort((a, b) => a - b);
  const at = (q) => s[Math.min(s.length - 1, Math.floor(q * s.length))];
  return { n: s.length, p50: at(0.5), p95: at(0.95), min: s[0], max: s[s.length - 1] };
}

async function run() {
  console.log(
    `==> Latency probe. profile=${DATA_DIR}  samples=${SAMPLES}  audio=${path.basename(AUDIO)}`,
  );
  if (
    await fetch(`http://127.0.0.1:${DEBUG_PORT}/json`).then(
      (r) => r.ok,
      () => false,
    )
  ) {
    die(`debug port ${DEBUG_PORT} already answering — set CORTEX_DEBUG_PORT.`);
  }

  wvDir = fs.mkdtempSync(path.join(os.tmpdir(), 'cortex-lat-wv-'));
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

  let pages = null;
  for (let i = 0; i < 120; i++) {
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
  if (!pages) throw new Error(`WebView2 debug port ${DEBUG_PORT} did not come up within 120s.`);

  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${DEBUG_PORT}`);
  const ctx = browser.contexts()[0];
  const page =
    ctx.pages().find((p) => p.url().includes('localhost') || p.url().includes('1420')) ||
    ctx.pages()[0];
  await page.waitForSelector('[data-testid="app-root"]', { timeout: 60000 });

  // PROVISION THE ENGINE FIRST, exactly as e2e_real_app.cjs does. A fresh disposable profile has no
  // ASR engine selected, and `import_audio_file` only SPAWNS the decode/VAD worker before returning
  // Ok — so a failed engine preflight surfaces on the event channel, never in the invoke's result. The
  // first version of this probe skipped it, polled 12 minutes for clips that could never arrive, and
  // the profile it kept for diagnosis showed the truth: zero segments, and jobs rows for exports only.
  // That looked exactly like "import silently does nothing", which would have been a serious and
  // WRONG bug report.
  const engine =
    process.env.CORTEX_GATE === '1' ? 'WSL7B' : process.env.CORTEX_ASR_ENGINE || 'WSL7B';
  await provisionEngine(page, DATA_DIR, engine);

  const measured = await page.evaluate(
    async ({ audio, samples, out, pace }) => {
      const invoke = window.__TAURI_INTERNALS__.invoke;
      const time = async (fn) => {
        const t0 = performance.now();
        try {
          await fn();
        } catch (e) {
          return { ms: performance.now() - t0, err: String(e) };
        }
        return { ms: performance.now() - t0 };
      };
      const results = {};
      const errors = {};
      const record = (name, r) => {
        (results[name] = results[name] || []).push(r.ms);
        if (r.err && !errors[name]) errors[name] = r.err;
      };

      // IMPORT, split into the two numbers that mean different things.
      //
      // `import_audio_file` RETURNS IMMEDIATELY — it enqueues, and Silero VAD segments in the
      // background (e2e_real_app polls up to 12 minutes for it). Reporting the invoke's duration as
      // "import latency" would publish the enqueue time, ~milliseconds, as if it were the wait a user
      // experiences. It is not; the wait is until clips appear.
      record('import_enqueue', await time(() => invoke('import_audio_file', { path: audio })));

      const tImport = performance.now();
      let seg = null;
      for (let i = 0; i < 360 && !seg; i++) {
        const p = await invoke('get_segments_page', {
          verified: null,
          query: null,
          sort: 'newest',
          limit: 50,
          cursor: null,
        }).catch(() => null);
        seg = (p && p.items && p.items[0]) || null;
        if (!seg) await new Promise((r) => setTimeout(r, 2000));
      }
      // The number a human actually waits on: audio in, first reviewable clip out.
      if (seg) record('import_to_first_clip', { ms: performance.now() - tImport });

      // SETTLE BEFORE SAMPLING. On a long file VAD keeps emitting clips in the background for minutes
      // after the first one. Sampling while the library grows measures a moving target: `library_page`
      // would page a table that is a different size on every iteration, and the run would not be
      // reproducible. Wait for the count to hold still, then record the size the numbers ran against —
      // a page latency is meaningless without saying how many rows it paged.
      let librarySize = 0;
      let settled = false;
      let lastCount = -1;
      let stable = 0;
      for (let i = 0; i < 900 && !settled; i++) {
        const p = await invoke('get_segments_page', {
          verified: null,
          query: null,
          sort: 'newest',
          limit: 1,
          cursor: null,
        }).catch(() => null);
        const n = p ? p.total : -1;
        stable = n >= 0 && n === lastCount ? stable + 1 : 0;
        lastCount = n;
        librarySize = n;
        if (stable >= 5) settled = true;
        else await new Promise((r) => setTimeout(r, 2000));
      }

      let probeSpeaker = seg ? (seg.speakerId ?? null) : null;

      for (let i = 0; i < samples; i++) {
        record(
          'library_page',
          await time(() =>
            invoke('get_segments_page', {
              verified: null,
              query: null,
              sort: 'newest',
              limit: 300,
              cursor: null,
            }),
          ),
        );
        record(
          'search',
          await time(() =>
            invoke('get_segments_page', {
              verified: null,
              query: 'ه',
              sort: 'newest',
              limit: 300,
              cursor: null,
            }),
          ),
        );
        if (seg) {
          // METADATA-SAVE: explicit per-field CAS, not a human review decision. Alternating values
          // proves both real writes and server-ACK rebasing rather than timing an idempotent no-op.
          const desiredSpeaker = `LATENCY_PROBE_${i % 2}`;
          record(
            'metadata_save',
            await time(async () => {
              const updated = await invoke('update_segment_metadata_v1', {
                request: {
                  segmentId: seg.id,
                  changes: [{ field: 'speakerId', expected: probeSpeaker, value: desiredSpeaker }],
                },
              });
              probeSpeaker = updated.speakerId;
            }),
          );
          record(
            'waveform',
            await time(() =>
              invoke('get_waveform', { path: audio, numPoints: 2000, alignmentJson: null }),
            ),
          );
        }
        record(
          'export_jsonl',
          await time(() =>
            invoke('export_dataset', { path: `${out}.${i}.jsonl`, format: 'jsonl' }),
          ),
        );

        // PACE BETWEEN SAMPLES. `update_segment_metadata_v1` and `export_dataset` sit behind the app's STRICT
        // token bucket (burst 5, refill 10/s — throttle.rs). Firing samples back to back with zero think
        // time drains that bucket and the 6th call returns "Rate limit exceeded". That is the limiter
        // working, NOT a defect: the editor autosaves on a 1s debounce, so a reviewer tops out near one
        // call/s per key against a limit of ten. An unpaced probe measures the throttle, not the app.
        //
        // The delay sits BETWEEN samples and never inside a timed region, so it cannot flatter a
        // percentile — it only stops the probe from generating load no human can.
        await new Promise((r) => setTimeout(r, pace));
      }
      return { results, errors, hadSegment: !!seg, librarySize, settled };
    },
    { audio: AUDIO, samples: SAMPLES, out: path.join(DATA_DIR, 'latency_export'), pace: PACE_MS },
  );

  await browser.close();
  killApp();

  const { results, errors, hadSegment, librarySize, settled } = measured;
  if (!hadSegment)
    throw new Error('import produced no segment — every per-segment op would be unmeasured');
  const fatal = Object.entries(errors);
  if (fatal.length) {
    // An operation that ERRORED was still fast, and reporting its speed would be a lie about a
    // feature that did not work. Fail loudly rather than publish a flattering number.
    throw new Error(
      `operations failed during measurement: ${fatal.map(([k, v]) => `${k}: ${v}`).join(' | ')}`,
    );
  }

  if (!settled) {
    // The count never held still, so the per-segment numbers paged a library of unknown, changing size.
    throw new Error('library never settled — per-segment latencies would be unreproducible');
  }

  // A PERCENTILE NEEDS SAMPLES. The import happens ONCE per probe run, so `import_enqueue` and
  // `import_to_first_clip` have n=1. Printing one observation under a "p50" and a "p95" column would
  // dress a single measurement up as a distribution — the reader sees p95 and believes the tail was
  // characterised when nothing about the tail is known. Split them out and label them as what they are.
  const MIN_PCTL_N = 3;
  const f = (v) => `${v.toFixed(1)}ms`.padStart(9);
  const dist = [];
  const single = [];
  for (const [name, samples] of Object.entries(results)) {
    (samples.length >= MIN_PCTL_N ? dist : single).push([name, stats(samples)]);
  }

  console.log(
    `\n  library: ${librarySize} segments   audio: ${path.basename(AUDIO)}   pacing: ${PACE_MS}ms`,
  );
  console.log('\n  operation             n     p50        p95        min        max');
  console.log('  ' + '-'.repeat(64));
  for (const [name, s] of dist) {
    console.log(
      `  ${name.padEnd(20)}${String(s.n).padStart(2)}  ${f(s.p50)}  ${f(s.p95)}  ${f(s.min)}  ${f(s.max)}`,
    );
  }
  if (single.length) {
    console.log('\n  single observations — one import per run, so NO percentile is defined:');
    for (const [name, s] of single) console.log(`  ${name.padEnd(22)}${f(s.p50)}   (n=${s.n})`);
  }
  console.log(
    `\nLATENCY OK: ${dist.length} operations over ${SAMPLES} samples each, ` +
      `${single.length} single-observation${single.length === 1 ? '' : 's'}.`,
  );
  console.log("(Reported, not gated — see this file's header for why.)");

  await cleanupProfile(DATA_DIR, OWNS_DATA_DIR);
  await cleanupProfile(wvDir, true);
}

run().catch((err) => {
  console.error('==> LATENCY PROBE FAILED:', err && err.message ? err.message : err);
  console.error(`==> Profiles kept for diagnosis: ${DATA_DIR}${wvDir ? ` and ${wvDir}` : ''}`);
  killApp();
  process.exit(1);
});
