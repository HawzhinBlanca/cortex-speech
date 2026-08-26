<script lang="ts">
  /**
   * RefineryPanel.svelte — surfaces the Disagreement Refinery's measured outputs that
   * previously rendered in ZERO components (blueprint M3.3): eval-run history (WER/CER per
   * run) and the escalation-rate trend. The raw-vs-jury label-quality LIFT card is pending
   * the M3.1 backend (computeLift) and is shown as a placeholder until then.
   */
  import { onMount } from 'svelte';
  import { chooseFile } from './fileDialogs';
  import * as api from './commands';
  import type { EvalRun, EscalationTrendPoint, LabelQualityLift, EvalRunResult } from './types';
  import { notifications } from './stores/notificationStore';
  import { isTauriRuntime } from './runtime';
  import { formatPublicErrorReference } from './errorText';
  import { t } from './i18n';

  let evalRuns: EvalRun[] = [];
  let trend: EscalationTrendPoint[] = [];
  let lift: LabelQualityLift | null = null;
  let loading = true;
  let error = '';

  // Eval-action state (legacy reactivity — this component is not in runes mode).
  const tauriAvailable = isTauriRuntime();
  let evalResult: EvalRunResult | null = null;
  let scorecardMd = '';
  let evalBusy = false;

  const pct = (x: number): string => `${(x * 100).toFixed(1)}%`;
  // FSI/PDI keeps interpolated model metrics and counts from reordering the surrounding Sorani text.
  const isolate = (value: string | number): string =>
    `${String.fromCodePoint(0x2068)}${String(value)}${String.fromCodePoint(0x2069)}`;
  // A WER/CER over ZERO scored segments is UNDEFINED, not 0% — the backend already refuses to read it as a
  // real rate (scorecard/render_markdown say "undefined (not 0%)"; the promotion gate returns "CANNOT
  // EVALUATE … undefined, not 0"). This panel is the one surface showing raw run.wer/run.cer, so mirror
  // that: an all-engine-fail run (numSegs=0, wer/cer persisted as 0.0) must show "—", never a perfect 0.0%.
  const metric = (x: number, numSegs: number): string => (numSegs > 0 ? pct(x) : '—');

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
      evalResult = await api.runGoldEvalAsr();
      await refreshRuns();
      notifications.success(
        $t('refinery.evalComplete', {
          cer: metric(evalResult.run.cer, evalResult.run.numSegs),
          n: String(evalResult.run.numSegs),
        }),
      );
    } catch (e) {
      notifications.error($t('refinery.evalFailed'), { cause: e });
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
      notifications.error($t('refinery.scorecardFailed'), { cause: e });
    } finally {
      evalBusy = false;
    }
  }

  async function importGoldFromFile() {
    if (evalBusy || !tauriAvailable) return;
    const picked = await chooseFile({ title: $t('refinery.selectVerifiedAudio') });
    if (!picked) return;
    evalBusy = true;
    try {
      const n = await api.createGoldFromFile(picked);
      notifications.success($t('refinery.goldCreated', { count: String(n) }));
    } catch (e) {
      notifications.error($t('refinery.goldCreateFailed'), { cause: e });
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
      error = $t('refinery.loadFailed', {
        error: formatPublicErrorReference(e) ?? $t('errors.unknown'),
      });
    } finally {
      loading = false;
    }
  });
</script>

<section class="refinery-panel" data-testid="refinery-panel" aria-label={$t('refinery.ariaLabel')}>
  <h2 class="panel-title">{$t('refinery.title')}</h2>

  {#if loading}
    <p class="muted" role="status">{$t('refinery.loading')}</p>
  {:else if error}
    <p class="error" role="alert">{error}</p>
  {:else}
    <!-- Label-quality lift (pending M3.1 backend) -->
    <div class="card" data-testid="refinery-lift">
      <h3 class="card-title">{$t('refinery.liftTitle')}</h3>
      {#if lift && lift.n > 0}
        <div class="lift-grid">
          <div class="lift-cell">
            <span class="lift-label">{$t('refinery.rawAsrCer')}</span>
            <bdi dir="ltr" class="lift-val">{pct(lift.rawMicroCer)}</bdi>
          </div>
          <div class="lift-cell">
            <span class="lift-label">{$t('refinery.postJuryCer')}</span>
            <bdi dir="ltr" class="lift-val">{pct(lift.juryMicroCer)}</bdi>
          </div>
          <div class="lift-cell">
            <span class="lift-label">{$t('refinery.cerLift')}</span>
            <bdi
              dir="ltr"
              class="lift-val"
              class:lift-pos={lift.cerLift > 0}
              class:lift-neg={lift.cerLift < 0}
            >
              {pct(lift.cerLift)}
            </bdi>
          </div>
        </div>
        <p class="muted" dir="auto">
          {$t('refinery.liftEvidence', {
            n: isolate(lift.n),
            low: isolate(pct(lift.liftCiLow)),
            high: isolate(pct(lift.liftCiHigh)),
          })}
        </p>
      {:else}
        <p class="muted">{$t('refinery.noMeasuredLift')}</p>
      {/if}
    </div>

    <!-- Champion eval, scorecard, and gold import. Auxiliary-engine eval stays offline-only. -->
    {#if tauriAvailable}
      <div class="card" data-testid="refinery-eval-actions">
        <h3 class="card-title">{$t('refinery.runEvaluation')}</h3>
        <div class="action-row">
          <button
            class="btn btn-secondary"
            onclick={runHonestCer}
            disabled={evalBusy}
            data-testid="eval-honest-cer"
          >
            {evalBusy ? $t('refinery.running') : $t('refinery.runHonestCer')}
          </button>
          <button
            class="btn btn-secondary"
            onclick={importGoldFromFile}
            disabled={evalBusy}
            data-testid="eval-import-gold"
          >
            {evalBusy ? $t('refinery.importing') : $t('refinery.importGold')}
          </button>
        </div>
        {#if evalResult}
          <p class="muted" data-testid="eval-result" dir="auto">
            {$t('refinery.lastEval', {
              cer: isolate(metric(evalResult.run.cer, evalResult.run.numSegs)),
              wer: isolate(metric(evalResult.run.wer, evalResult.run.numSegs)),
              n: isolate(evalResult.run.numSegs),
            })}
            <button
              class="btn btn-ghost"
              onclick={buildScorecardFromResult}
              disabled={evalBusy}
              data-testid="eval-build-scorecard"
            >
              {$t('refinery.buildScorecard')}
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
      <h3 class="card-title">{$t('refinery.evaluationRuns')}</h3>
      {#if evalRuns.length === 0}
        <p class="muted">{$t('refinery.noEvalRuns')}</p>
      {:else}
        <table class="metrics-table">
          <thead>
            <tr
              ><th>{$t('refinery.model')}</th><th>{$t('refinery.when')}</th><th>N</th><th>WER</th
              ><th>CER</th></tr
            >
          </thead>
          <tbody>
            {#each evalRuns as run (run.id)}
              <tr>
                <td><bdi>{run.modelId}</bdi></td>
                <td><bdi>{run.runAt}</bdi></td>
                <td><bdi dir="ltr">{run.numSegs}</bdi></td>
                <td><bdi dir="ltr">{metric(run.wer, run.numSegs)}</bdi></td>
                <td><bdi dir="ltr">{metric(run.cer, run.numSegs)}</bdi></td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>

    <!-- Escalation-rate trend -->
    <div class="card" data-testid="refinery-escalation-trend">
      <h3 class="card-title">{$t('refinery.escalationTrend')}</h3>
      {#if trend.length === 0}
        <p class="muted">{$t('refinery.noEscalationHistory')}</p>
      {:else}
        <ul class="trend-list">
          {#each trend as point (point.date)}
            <li
              class="trend-row"
              aria-label={$t('refinery.trendPoint', {
                date: isolate(point.date),
                rate: isolate(pct(point.escalationRate)),
                escalated: isolate(point.escalated),
                total: isolate(point.total),
              })}
            >
              <span class="trend-date" aria-hidden="true"><bdi dir="ltr">{point.date}</bdi></span>
              <span class="trend-bar-wrap" aria-hidden="true">
                <span class="trend-bar" style="width:{Math.min(100, point.escalationRate * 100)}%"
                ></span>
              </span>
              <bdi class="trend-val" dir="ltr" aria-hidden="true"
                >{pct(point.escalationRate)} ({point.escalated}/{point.total})</bdi
              >
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
