<script lang="ts">
  /**
   * RefineryPanel.svelte — surfaces the Disagreement Refinery's measured outputs that
   * previously rendered in ZERO components (blueprint M3.3): eval-run history (WER/CER per
   * run) and the escalation-rate trend. The raw-vs-jury label-quality LIFT card is pending
   * the M3.1 backend (computeLift) and is shown as a placeholder until then.
   */
  import { onMount } from 'svelte';
  import * as api from './commands';
  import type { EvalRun, EscalationTrendPoint } from './types';

  let evalRuns: EvalRun[] = [];
  let trend: EscalationTrendPoint[] = [];
  let loading = true;
  let error = '';

  const pct = (x: number): string => `${(x * 100).toFixed(1)}%`;

  onMount(async () => {
    try {
      [evalRuns, trend] = await Promise.all([api.listEvalRuns(), api.getEscalationRateTrend()]);
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
      <p class="muted">
        Pending M3.1: a measured raw-vs-jury CER delta with a bootstrap CI on the difference.
      </p>
    </div>

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
</style>
