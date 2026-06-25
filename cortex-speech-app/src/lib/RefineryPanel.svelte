<script lang="ts">
  /**
   * RefineryPanel.svelte — surfaces the Disagreement Refinery's measured outputs that
   * previously rendered in ZERO components (blueprint M3.3): eval-run history (WER/CER per
   * run) and the escalation-rate trend. The raw-vs-jury label-quality LIFT card is pending
   * the M3.1 backend (computeLift) and is shown as a placeholder until then.
   */
  import { onMount } from 'svelte';
  import * as api from './commands';
  import type { EvalRun, EscalationTrendPoint, LabelQualityLift, EvalRunResult } from './types';
  import { notifications } from './stores/notificationStore';
  import { isTauriRuntime } from './runtime';

  let evalRuns: EvalRun[] = [];
  let trend: EscalationTrendPoint[] = [];
  let lift: LabelQualityLift | null = null;
  let loading = true;
  let error = '';

  // Eval-action state (legacy reactivity — this component is not in runes mode).
  const tauriAvailable = isTauriRuntime();
  let evalModelId = '';
  let evalResult: EvalRunResult | null = null;
  let scorecardMd = '';
  let evalBusy = false;

  const pct = (x: number): string => `${(x * 100).toFixed(1)}%`;

  async function refreshRuns() {
    try {
      evalRuns = await api.listEvalRuns();
    } catch {
      // keep the previous list on a transient refresh failure
    }
  }

  async function runHonestCer() {
    if (evalBusy || !tauriAvailable) return;
    evalBusy = true;
    scorecardMd = '';
    try {
      evalResult = await api.runGoldEvalAsr(evalModelId.trim() || null);
      await refreshRuns();
      notifications.success(
        `Honest-CER eval done: CER ${pct(evalResult.run.cer)} (N=${evalResult.run.numSegs}).`,
      );
    } catch (e) {
      notifications.error('Honest-CER eval failed', { detail: String(e) });
    } finally {
      evalBusy = false;
    }
  }

  async function runLocalEval() {
    if (evalBusy || !tauriAvailable || !evalModelId.trim()) return;
    evalBusy = true;
    scorecardMd = '';
    try {
      evalResult = await api.runGoldEvalLocal(evalModelId.trim());
      await refreshRuns();
      notifications.success(
        `Local eval done: CER ${pct(evalResult.run.cer)} (N=${evalResult.run.numSegs}).`,
      );
    } catch (e) {
      notifications.error('Local eval failed', { detail: String(e) });
    } finally {
      evalBusy = false;
    }
  }

  async function buildScorecardFromResult() {
    if (!evalResult || evalBusy) return;
    evalBusy = true;
    try {
      const res = await api.buildScorecard(evalResult);
      scorecardMd = res.markdown;
    } catch (e) {
      notifications.error('Build scorecard failed', { detail: String(e) });
    } finally {
      evalBusy = false;
    }
  }

  async function importGoldFromFile() {
    if (evalBusy || !tauriAvailable) return;
    const { open } = await import('@tauri-apps/plugin-dialog');
    const picked = await open({ multiple: false, title: 'Select a verified audio file' });
    if (typeof picked !== 'string') return;
    evalBusy = true;
    try {
      const n = await api.createGoldFromFile(picked);
      notifications.success(`Created ${n} gold segment(s) from the file.`);
    } catch (e) {
      notifications.error('Create gold from file failed', { detail: String(e) });
    } finally {
      evalBusy = false;
    }
  }

  onMount(async () => {
    try {
      [evalRuns, trend, lift] = await Promise.all([
        api.listEvalRuns(),
        api.getEscalationRateTrend(),
        api.getLabelQualityLift(),
      ]);
    } catch (e) {
      error = `Failed to load refinery data: ${e}`;
    } finally {
      loading = false;
    }
  });
</script>

<section class="refinery-panel" data-testid="refinery-panel" aria-label="Refinery metrics">
  <h2 class="panel-title">Disagreement Refinery</h2>

  {#if loading}
    <p class="muted">Loading refinery metrics…</p>
  {:else if error}
    <p class="error" role="alert">{error}</p>
  {:else}
    <!-- Label-quality lift (pending M3.1 backend) -->
    <div class="card" data-testid="refinery-lift">
      <h3 class="card-title">Label-quality lift (raw ASR → post-jury)</h3>
      {#if lift && lift.n > 0}
        <div class="lift-grid">
          <div class="lift-cell">
            <span class="lift-label">Raw ASR CER</span>
            <span class="lift-val">{pct(lift.rawMicroCer)}</span>
          </div>
          <div class="lift-cell">
            <span class="lift-label">Post-jury CER</span>
            <span class="lift-val">{pct(lift.juryMicroCer)}</span>
          </div>
          <div class="lift-cell">
            <span class="lift-label">CER lift</span>
            <span class="lift-val" class:lift-pos={lift.cerLift > 0} class:lift-neg={lift.cerLift < 0}>
              {pct(lift.cerLift)}
            </span>
          </div>
        </div>
        <p class="muted">
          n={lift.n} verified segments · 95% CI [{pct(lift.liftCiLow)}, {pct(lift.liftCiHigh)}]
        </p>
      {:else}
        <p class="muted">
          No measured lift yet — needs human-verified segments that also carry a jury verdict.
        </p>
      {/if}
    </div>

    <!-- Eval actions (honest-CER / local eval, scorecard, gold-from-file) -->
    {#if tauriAvailable}
      <div class="card" data-testid="refinery-eval-actions">
        <h3 class="card-title">Run evaluation</h3>
        <input
          class="input eval-input"
          placeholder="model id (optional for honest-CER)"
          aria-label="Eval model id"
          bind:value={evalModelId}
        />
        <div class="action-row">
          <button
            class="btn btn-secondary"
            onclick={runHonestCer}
            disabled={evalBusy}
            data-testid="eval-honest-cer"
          >
            {evalBusy ? 'Running…' : 'Run honest-CER eval'}
          </button>
          <button
            class="btn btn-secondary"
            onclick={runLocalEval}
            disabled={evalBusy || !evalModelId.trim()}
            data-testid="eval-local"
          >
            Run local-pipeline eval
          </button>
          <button
            class="btn btn-secondary"
            onclick={importGoldFromFile}
            disabled={evalBusy}
            data-testid="eval-import-gold"
          >
            Import gold from file…
          </button>
        </div>
        {#if evalResult}
          <p class="muted" data-testid="eval-result">
            Last eval: CER {pct(evalResult.run.cer)} · WER {pct(evalResult.run.wer)} · N={evalResult
              .run.numSegs}
            <button
              class="btn btn-ghost"
              onclick={buildScorecardFromResult}
              disabled={evalBusy}
              data-testid="eval-build-scorecard"
            >
              Build scorecard
            </button>
          </p>
        {/if}
        {#if scorecardMd}
          <pre class="scorecard" data-testid="eval-scorecard">{scorecardMd}</pre>
        {/if}
      </div>
    {/if}

    <!-- Eval-run history -->
    <div class="card" data-testid="refinery-eval-runs">
      <h3 class="card-title">Evaluation runs</h3>
      {#if evalRuns.length === 0}
        <p class="muted">No eval runs yet — run the gold eval to populate this.</p>
      {:else}
        <table class="metrics-table">
          <thead>
            <tr><th>Model</th><th>When</th><th>N</th><th>WER</th><th>CER</th></tr>
          </thead>
          <tbody>
            {#each evalRuns as run (run.id)}
              <tr>
                <td><bdi>{run.modelId}</bdi></td>
                <td><bdi>{run.runAt}</bdi></td>
                <td>{run.numSegs}</td>
                <td>{pct(run.wer)}</td>
                <td>{pct(run.cer)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>

    <!-- Escalation-rate trend -->
    <div class="card" data-testid="refinery-escalation-trend">
      <h3 class="card-title">Escalation rate trend</h3>
      {#if trend.length === 0}
        <p class="muted">No escalation history yet.</p>
      {:else}
        <ul class="trend-list">
          {#each trend as point (point.date)}
            <li class="trend-row">
              <span class="trend-date"><bdi>{point.date}</bdi></span>
              <span class="trend-bar-wrap" aria-hidden="true">
                <span class="trend-bar" style="width:{Math.min(100, point.escalationRate * 100)}%"></span>
              </span>
              <span class="trend-val">{pct(point.escalationRate)} ({point.escalated}/{point.total})</span>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
</section>

<style>
  .refinery-panel {
    padding: 16px;
    color: var(--text);
  }
  .panel-title {
    font-size: 1.1rem;
    font-weight: 600;
    margin-bottom: 12px;
  }
  .card {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 12px 16px;
    margin-bottom: 12px;
  }
  .card-title {
    font-size: 0.9rem;
    font-weight: 600;
    margin-bottom: 8px;
  }
  .muted {
    color: var(--text-muted);
    font-size: 0.85rem;
  }
  .error {
    color: var(--error, #c0392b);
    font-size: 0.85rem;
  }
  .lift-grid {
    display: flex;
    gap: 16px;
    margin-bottom: 6px;
  }
  .lift-cell {
    display: flex;
    flex-direction: column;
  }
  .lift-label {
    font-size: 0.7rem;
    color: var(--text-muted);
  }
  .lift-val {
    font-size: 1rem;
    font-weight: 600;
  }
  .lift-pos {
    color: var(--success, #2e7d32);
  }
  .lift-neg {
    color: var(--error, #c0392b);
  }
  .metrics-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.8rem;
  }
  .metrics-table th,
  .metrics-table td {
    text-align: left;
    padding: 4px 8px;
    border-bottom: 1px solid var(--border);
  }
  .trend-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .trend-row {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.8rem;
    padding: 3px 0;
  }
  .trend-date {
    width: 90px;
    color: var(--text-muted);
  }
  .trend-bar-wrap {
    flex: 1;
    height: 8px;
    background: var(--border);
    border-radius: 4px;
    overflow: hidden;
  }
  .trend-bar {
    display: block;
    height: 100%;
    background: var(--accent, #3b82f6);
  }
  .trend-val {
    width: 120px;
    text-align: right;
  }
  .eval-input {
    width: 100%;
    margin-bottom: 8px;
  }
  .action-row {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .scorecard {
    margin-top: 8px;
    max-height: 240px;
    overflow: auto;
    font-size: 0.72rem;
    white-space: pre-wrap;
    word-break: break-word;
    background: var(--surface-1, var(--surface-2));
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px;
  }
</style>
