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
  import RefineryEvidence from './RefineryEvidence.svelte';
  import { formatRefineryMetric, isolateRefineryValue } from './refineryPresentation';

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
          cer: formatRefineryMetric(evalResult.run.cer, evalResult.run.numSegs),
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
    <RefineryEvidence {evalRuns} {trend} {lift} />

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
              cer: isolateRefineryValue(
                formatRefineryMetric(evalResult.run.cer, evalResult.run.numSegs),
              ),
              wer: isolateRefineryValue(
                formatRefineryMetric(evalResult.run.wer, evalResult.run.numSegs),
              ),
              n: isolateRefineryValue(evalResult.run.numSegs),
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
