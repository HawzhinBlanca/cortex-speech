/**
 * Disposable-profile launch guard for the IPC e2e harnesses.
 *
 * WHY THIS EXISTS. The real-exe IPC harnesses once spawned with `{ ...process.env }` and nothing
 * else — no `CORTEX_APP_DATA_DIR` — so they ran against
 * the owner's REAL `%APPDATA%\cortex-speech` library and imported audio straight into a corpus that
 * holds human review decisions. They also killed by IMAGE NAME (`taskkill /F /IM`), which takes down
 * the owner's own running Cortex, and left the WebView2 profile shared, which is the documented cause
 * of a launch that times out on a debug port nobody opened whenever his app happens to be open.
 *
 * `e2e_real_app.cjs` had all three protections. They were written for that one harness while its
 * sibling was left behind — the same shape as a guard applied at one call site instead of the
 * shared one.
 *
 * NOT SHARED WITH `e2e_real_app.cjs`, deliberately. That harness is the only one wired into a gate
 * (verify-10, package.json, ci.yml), it works, and `test_real_data_runner_policy.py` pins its guards
 * literal by literal. Rewriting the one harness that actually gates the repo so it can share code
 * with a diagnostic harness is the wrong risk trade. This module gives it the same
 * protections; the policy now checks both.
 */
const { execFileSync, execSync } = require('child_process');
const path = require('path');
const fs = require('fs');
const os = require('os');

/** The production profile this must never touch. */
const PROD_PROFILE = process.env.APPDATA ? path.join(process.env.APPDATA, 'cortex-speech') : null;
const normPath = (p) =>
  path
    .resolve(p)
    .replace(/[\\/]+$/, '')
    .toLowerCase();

/**
 * Resolve the profile this run may use, refusing the production one outright.
 *
 * A caller-supplied `CORTEX_APP_DATA_DIR` is honoured (so a post-mortem can point a run at a kept
 * profile) but is validated the same way: naming the real library explicitly is not permission, it is
 * the mistake this refuses.
 */
function resolveDisposableProfile(harness) {
  const supplied = process.env.CORTEX_APP_DATA_DIR;
  if (supplied) {
    if (
      PROD_PROFILE &&
      (normPath(supplied) === normPath(PROD_PROFILE) ||
        normPath(supplied).startsWith(normPath(PROD_PROFILE) + path.sep))
    ) {
      console.error(
        `REFUSED: CORTEX_APP_DATA_DIR points at the REAL profile (${PROD_PROFILE}). ` +
          `${harness} must never run against the production library — use a disposable directory.`,
      );
      process.exit(1);
    }
    return { dataDir: supplied, ours: false };
  }
  return { dataDir: fs.mkdtempSync(path.join(os.tmpdir(), 'cortex-e2e-')), ours: true };
}

/**
 * The env a spawned app must be given: its own library, its own browser profile, its own debug port.
 *
 * WebView2 keys its profile on the BUNDLE IDENTITY, not on `CORTEX_APP_DATA_DIR`, so without the
 * folder override a run launched while the owner's Cortex is open shares that folder, WebView2 fails
 * the environment (HRESULT 0x8007139F) and silently drops `--remote-debugging-port`.
 */
function launchEnv(dataDir, debugPort) {
  const webview2 = path.join(dataDir, 'webview2');
  fs.mkdirSync(webview2, { recursive: true });
  return {
    ...process.env,
    CORTEX_APP_DATA_DIR: dataDir,
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${debugPort}`,
    WEBVIEW2_USER_DATA_FOLDER: webview2,
  };
}

/**
 * Kill ONLY the process tree we spawned. Never `taskkill /IM` by image name — that is indiscriminate
 * and would kill the Cortex the owner is using. No-op when nothing was spawned.
 */
function killSpawned(child) {
  if (!child || !child.pid) return;
  try {
    execSync(`taskkill /F /T /PID ${child.pid}`, { stdio: 'ignore' });
  } catch (e) {
    /* already gone */
  }
}

/**
 * Remove the profile — only one we minted, only under the temp root, and only on SUCCESS.
 *
 * Kept on failure on purpose: the profile holds the only copy of the DB a post-mortem can read, and a
 * harness that destroys its own evidence is worse than one that leaves a directory behind.
 */
async function cleanupProfile(dataDir, ours) {
  if (!ours) return;
  const root = path.resolve(os.tmpdir());
  const target = path.resolve(dataDir);
  if (target === root || !target.startsWith(root + path.sep)) return;
  // Windows releases file handles ASYNCHRONOUSLY: taskkill returns when the kill is signalled, not
  // when the app's SQLite handles and its msedgewebview2 children have let go. A single rmSync
  // straight after the kill fails every time — measured on e2e_real_app, where exactly this left 34
  // stale profiles totalling 764 MB. Retry to a deadline instead.
  const deadline = Date.now() + 15000;
  for (;;) {
    try {
      fs.rmSync(target, { recursive: true, force: true });
      console.log(`==> Removed the disposable profile ${target}`);
      return;
    } catch (e) {
      if (Date.now() >= deadline) {
        // Non-fatal: a leaked temp directory must never turn a passing run into a failure.
        console.log(`==> Could not remove the disposable profile ${target}: ${e.message}`);
        return;
      }
      await new Promise((r) => setTimeout(r, 500));
    }
  }
}

/**
 * Refuse to start when the debug port is ALREADY answering.
 *
 * These harnesses used to clear the way with `taskkill /IM`, which killed the owner's Cortex along
 * with any stale instance. Now that they only kill what they spawned, a leftover from a previous run
 * would hold the port and the harness would sit through a 90-second poll before reporting a launch
 * timeout — blaming the app for a busy port. Say so up front instead.
 */
async function refuseIfDebugPortBusy(debugPort, harness) {
  const answering = await fetch(`http://localhost:${debugPort}/json`).then(
    (r) => r.ok,
    () => false,
  );
  if (answering) {
    console.error(
      `PRECONDITION FAILED: debug port ${debugPort} is already answering — a previous instance is ` +
        `still running. Close it, or set CORTEX_DEBUG_PORT to a free port. ${harness} only kills ` +
        'processes it spawned.',
    );
    process.exit(1);
  }
}

/**
 * Point the DISPOSABLE profile at an explicitly selected ASR engine before importing anything.
 *
 * A fresh profile takes the app's default engine, which is the OmniASR-7B champion — and that needs
 * the owner's warm WSL 7B server. These harnesses never had to think about it because they ran
 * against his real profile, which already has a working engine configured; isolating them exposed
 * the dependency they had been borrowing. Measured: with no provisioning, `e2e_pipeline_ipc` sat in
 * its `get_segments` poll with nothing ever arriving, and would have blamed VAD for an import that
 * could not decode.
 *
 * The standard proof path uses WSL7B. Smaller engines remain available only when a diagnostic
 * harness names one explicitly; this helper never substitutes one because the champion is busy or
 * unavailable. WSL7B setup mirrors production identity checks while keeping the test database and
 * settings fully disposable.
 */
async function provisionEngine(page, dataDir, engine = 'WSL7B') {
  if (!dataDir) throw new Error('provisionEngine requires the disposable profile path');
  if (
    PROD_PROFILE &&
    (normPath(dataDir) === normPath(PROD_PROFILE) ||
      normPath(dataDir).startsWith(normPath(PROD_PROFILE) + path.sep))
  ) {
    throw new Error(`REFUSED: provisionEngine was given the REAL profile (${PROD_PROFILE})`);
  }

  const championScript = path.join(__dirname, 'scripts', 'cortex_7b_client.py');
  await page
    .evaluate(
      async ({ selectedEngine, clientScript }) => {
        const s = await window.__TAURI_INTERNALS__.invoke('get_settings');
        s.asr_model_size = selectedEngine;
        // Refinement OFF, explicitly. The default is llm_mode = Local, which needs an LLM server on
        // 127.0.0.1 that this machine may or may not be running — and a refinement failure is a HARD
        // STOP by design, so the whole gate then fails for a reason that has nothing to do with what
        // it proves (import -> VAD -> ASR). A gate must own every setting its verdict depends on.
        // Measured 2026-08-17: Ollama answered 404 and three green pipelines reported dead.
        s.llm_mode = 'None';
        s.multi_engine_hypotheses = false;
        s.use_finetuned_asr = false;
        s.cloud_stt_opt_in = false;
        s.cloud_llm_opt_in = false;
        s.jury_cloud_opt_in = false;
        s.champion_supervision_enabled = false;
        if (selectedEngine === 'WSL7B') s.external_asr_script_path = clientScript;
        await window.__TAURI_INTERNALS__.invoke('update_settings', { settings: s });
      },
      { selectedEngine: engine, clientScript: championScript },
    )
    .catch((e) => {
      throw new Error('Could not provision the ASR engine in the disposable profile: ' + e.message);
    });

  if (engine === 'WSL7B') {
    const pointerPath =
      process.env.CORTEX_CHAMPION_POINTER ||
      (process.env.APPDATA ? path.join(process.env.APPDATA, 'cortex-speech', 'champion.json') : '');
    if (!pointerPath || !fs.existsSync(pointerPath)) {
      throw new Error(
        'OmniASR-7B proof requires the live champion pointer; set CORTEX_CHAMPION_POINTER to champion.json',
      );
    }
    const pointer = JSON.parse(fs.readFileSync(pointerPath, 'utf-8')).champions?.['omniasr-7b'];
    for (const key of ['modelVersionId', 'deploymentSha256', 'deploymentManifestPath']) {
      if (!pointer || typeof pointer[key] !== 'string' || !pointer[key].trim()) {
        throw new Error(`champion.json has no valid champions.omniasr-7b.${key}`);
      }
    }
    const dbPath = path.join(dataDir, 'cortex-speech.db');
    if (!fs.existsSync(dbPath)) throw new Error(`disposable Cortex database is missing: ${dbPath}`);
    const code = [
      'import sqlite3, sys',
      'db, model_id, deployment_sha, manifest = sys.argv[1:]',
      'con = sqlite3.connect(db, timeout=30)',
      'con.execute("PRAGMA busy_timeout=30000")',
      'try:',
      '    con.execute("BEGIN IMMEDIATE")',
      "    con.execute(\"UPDATE model_versions SET status = 'rolled_back' WHERE family = 'omniasr-7b' AND status = 'champion' AND id <> ?\", (model_id,))",
      "    con.execute(\"INSERT OR REPLACE INTO model_versions (id, family, model_card_name, checkpoint_sha256, checkpoint_path, source, license, status) VALUES (?, 'omniasr-7b', 'soranivoice_omniASR_LLM_7B_v2_local', ?, ?, 'user-finetuned', 'owner-full-rights', 'champion')\", (model_id, deployment_sha, manifest))",
      '    con.commit()',
      'except:',
      '    con.rollback()',
      '    raise',
      'finally:',
      '    con.close()',
    ].join('\n');
    execFileSync(
      process.env.PYTHON || 'python',
      [
        '-c',
        code,
        dbPath,
        pointer.modelVersionId,
        pointer.deploymentSha256,
        pointer.deploymentManifestPath,
      ],
      { stdio: 'pipe' },
    );
  }
  console.log(`==> Provisioned ASR engine '${engine}' (refinement off) in the disposable profile`);
}

module.exports = {
  resolveDisposableProfile,
  launchEnv,
  killSpawned,
  cleanupProfile,
  refuseIfDebugPortBusy,
  provisionEngine,
};
