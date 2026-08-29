<script lang="ts">
  import { t } from './i18n';
  import {
    formatRefineryMetric,
    formatRefineryPercent,
    isolateRefineryValue,
  } from './refineryPresentation';
  import type { EscalationTrendPoint, EvalRun, LabelQualityLift } from './types';

  let {
    evalRuns,
    trend,
    lift,
  }: { evalRuns: EvalRun[]; trend: EscalationTrendPoint[]; lift: LabelQualityLift | null } =
    $props();
</script>

<div class="card" data-testid="refinery-lift">
  <h3 class="card-title">{$t('refinery.liftTitle')}</h3>
  {#if lift && lift.n > 0}
    <div class="lift-grid">
      <div class="lift-cell">
        <span class="lift-label">{$t('refinery.rawAsrCer')}</span>
        <bdi dir="ltr" class="lift-val">{formatRefineryPercent(lift.rawMicroCer)}</bdi>
      </div>
      <div class="lift-cell">
        <span class="lift-label">{$t('refinery.postJuryCer')}</span>
        <bdi dir="ltr" class="lift-val">{formatRefineryPercent(lift.juryMicroCer)}</bdi>
      </div>
      <div class="lift-cell">
        <span class="lift-label">{$t('refinery.cerLift')}</span>
        <bdi
          dir="ltr"
          class="lift-val"
          class:lift-pos={lift.cerLift > 0}
          class:lift-neg={lift.cerLift < 0}
        >
          {formatRefineryPercent(lift.cerLift)}
        </bdi>
      </div>
    </div>
    <p class="muted" dir="auto">
      {$t('refinery.liftEvidence', {
        n: isolateRefineryValue(lift.n),
        low: isolateRefineryValue(formatRefineryPercent(lift.liftCiLow)),
        high: isolateRefineryValue(formatRefineryPercent(lift.liftCiHigh)),
      })}
    </p>
  {:else}
    <p class="muted">{$t('refinery.noMeasuredLift')}</p>
  {/if}
</div>

<div class="card" data-testid="refinery-eval-runs">
  <h3 class="card-title">{$t('refinery.evaluationRuns')}</h3>
  {#if evalRuns.length === 0}
    <p class="muted">{$t('refinery.noEvalRuns')}</p>
  {:else}
    <table class="metrics-table">
      <thead>
        <tr
          ><th>{$t('refinery.model')}</th><th>{$t('refinery.when')}</th><th>N</th><th>WER</th><th
            >CER</th
          ></tr
        >
      </thead>
      <tbody>
        {#each evalRuns as run (run.id)}
          <tr>
            <td><bdi>{run.modelId}</bdi></td>
            <td><bdi>{run.runAt}</bdi></td>
            <td><bdi dir="ltr">{run.numSegs}</bdi></td>
            <td><bdi dir="ltr">{formatRefineryMetric(run.wer, run.numSegs)}</bdi></td>
            <td><bdi dir="ltr">{formatRefineryMetric(run.cer, run.numSegs)}</bdi></td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

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
            date: isolateRefineryValue(point.date),
            rate: isolateRefineryValue(formatRefineryPercent(point.escalationRate)),
            escalated: isolateRefineryValue(point.escalated),
            total: isolateRefineryValue(point.total),
          })}
        >
          <span class="trend-date" aria-hidden="true"><bdi dir="ltr">{point.date}</bdi></span>
          <span class="trend-bar-wrap" aria-hidden="true">
            <span class="trend-bar" style="width:{Math.min(100, point.escalationRate * 100)}%"
            ></span>
          </span>
          <bdi class="trend-val" dir="ltr" aria-hidden="true"
            >{formatRefineryPercent(point.escalationRate)} ({point.escalated}/{point.total})</bdi
          >
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
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
</style>
