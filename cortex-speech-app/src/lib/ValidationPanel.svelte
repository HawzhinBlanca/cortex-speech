<script lang="ts">
  import { onMount } from 'svelte';
  import { focusTrap } from './actions/focusTrap';
  import * as api from './commands';
  import type { ValidationIssue } from './commands';
  import type { SpeechSegment } from './types';
  import { showValidationPanel } from './stores/uiStore';
  import { notifications } from './stores/notificationStore';
  import { selectedSegmentId } from './stores/segmentStore';
  import { t } from './i18n';
  import ValidationActiveLearningTab from './ValidationActiveLearningTab.svelte';
  import ValidationDatasetTab from './ValidationDatasetTab.svelte';
  import ValidationSignalAnomalyTab from './ValidationSignalAnomalyTab.svelte';

  let loading = $state(true);
  let summary = $state('');
  let errors = $state<ValidationIssue[]>([]);
  let warnings = $state<ValidationIssue[]>([]);
  let totalSegments = $state(0);
  let passed = $state(0);
  let showErrors = $state(true);
  let showWarnings = $state(true);

  // Tabs: 'dataset' | 'active' | 'signalAnomaly'
  let activeTab = $state<'dataset' | 'active' | 'signalAnomaly'>('dataset');

  // Active learning parameters & queue
  let targetError = $state(0.05);
  let confidenceLevel = $state(0.95);
  let queueLimit = $state(20);
  let activeQueueLoading = $state(false);
  let activeQueue = $state<SpeechSegment[]>([]);

  // Signal-anomaly status, running status, and flagged segments list
  let signalAnomalyRunning = $state(false);
  let signalAnomalySegments = $state<SpeechSegment[]>([]);
  let showAllSegmentsForSignalAnomaly = $state(false);

  async function runValidation() {
    loading = true;
    try {
      const result = await api.validateDataset();
      if (!result) throw new Error('Validation returned no result');
      summary = result.summary ?? '';
      errors = result.errors ?? [];
      warnings = result.warnings ?? [];
      totalSegments = result.totalSegments ?? 0;
      passed = result.passed ?? 0;
    } catch (e) {
      notifications.error($t('validation.failed'), { cause: e });
      showValidationPanel.set(false);
    } finally {
      loading = false;
    }
  }

  async function loadActiveLearningQueue() {
    activeQueueLoading = true;
    // Clamp user inputs — HTML min/max don't prevent typed values, and a CLEARED field binds NaN
    // (Math.floor(NaN)=NaN slips through Math.max/min and reaches the IPC call). NaN-safe clamp with a
    // sensible default; the limit floor is 5 to match the input's min="5".
    const clamp = (v: number, lo: number, hi: number, dflt: number): number =>
      Number.isFinite(v) ? Math.max(lo, Math.min(hi, v)) : dflt;
    const clampedTarget = clamp(targetError, 0.01, 0.5, 0.1);
    const clampedConf = clamp(confidenceLevel, 0.5, 0.99, 0.95);
    const clampedLimit = clamp(Math.floor(queueLimit), 5, 100, 20);
    try {
      const queue = await api.getActiveLearningQueue(clampedTarget, clampedConf, clampedLimit);
      activeQueue = queue;
    } catch (e) {
      notifications.error($t('validation.failed'), { cause: e });
    } finally {
      activeQueueLoading = false;
    }
  }

  async function runSignalAnomalyDetection() {
    signalAnomalyRunning = true;
    try {
      const count = await api.computeSignalAnomalyScores();
      notifications.success($t('validation.signalAnomaly.success').replace('{n}', String(count)));
      await reloadSignalAnomalySegments();
    } catch (e) {
      notifications.error($t('validation.failed'), { cause: e });
    } finally {
      signalAnomalyRunning = false;
    }
  }

  async function reloadSignalAnomalySegments() {
    try {
      signalAnomalySegments = await api.getSignalAnomalySegments(100);
    } catch (e) {
      console.error('Failed to load signal-anomaly segments', e);
      notifications.error($t('validation.failed'), { cause: e });
    }
  }

  function close() {
    showValidationPanel.set(false);
  }

  function jumpToSegment(issue: ValidationIssue) {
    if (issue.segmentId) {
      selectedSegmentId.set(issue.segmentId);
      close();
    }
  }

  function jumpToSegmentId(segmentId: string) {
    selectedSegmentId.set(segmentId);
    close();
  }

  onMount(() => {
    runValidation();
  });
</script>

<div
  data-testid="validation-panel"
  class="fixed inset-0 z-50 flex items-start justify-center pt-16 px-4 bg-black/60 backdrop-blur-sm"
  role="dialog"
  aria-modal="true"
  aria-labelledby="validation-title"
  tabindex="-1"
  use:focusTrap
  onclick={(e) => {
    if (e.target === e.currentTarget) close();
  }}
  onkeydown={(e) => {
    if (e.key === 'Escape') close();
  }}
>
  <div class="card w-full max-w-2xl max-h-[80vh] flex flex-col shadow-2xl">
    <header
      class="flex items-center justify-between px-4 py-3 border-b border-cortex-800/50 shrink-0"
    >
      <div>
        <h2 id="validation-title" class="text-sm font-semibold text-cortex-100">
          {$t('validation.title')}
        </h2>
        {#if !loading && summary && activeTab === 'dataset'}
          <p class="text-xs text-cortex-400 mt-0.5">{summary}</p>
        {/if}
      </div>
      <button class="btn-secondary !text-xs !px-2 !py-1" onclick={close}>
        {$t('close')}
      </button>
    </header>

    <!-- Navigation Tabs -->
    <div class="flex border-b border-cortex-800/50 bg-cortex-900/40 p-1 shrink-0 gap-1">
      <button
        type="button"
        class="flex-1 py-1.5 text-xs font-semibold rounded-md transition-all duration-200 border-0 cursor-pointer
          {activeTab === 'dataset'
          ? 'bg-cortex-700 text-default shadow-sm font-bold'
          : 'bg-transparent text-cortex-400 hover:text-cortex-200 hover:bg-cortex-800/30'}"
        onclick={() => (activeTab = 'dataset')}
      >
        {$t('validation.tab.dataset')}
      </button>
      <button
        type="button"
        class="flex-1 py-1.5 text-xs font-semibold rounded-md transition-all duration-200 border-0 cursor-pointer
          {activeTab === 'active'
          ? 'bg-cortex-700 text-default shadow-sm font-bold'
          : 'bg-transparent text-cortex-400 hover:text-cortex-200 hover:bg-cortex-800/30'}"
        onclick={() => {
          const wasActive = activeTab === 'active';
          activeTab = 'active';
          if (!wasActive) loadActiveLearningQueue();
        }}
      >
        {$t('validation.tab.activeLearning')}
      </button>
      <button
        type="button"
        class="flex-1 py-1.5 text-xs font-semibold rounded-md transition-all duration-200 border-0 cursor-pointer
          {activeTab === 'signalAnomaly'
          ? 'bg-cortex-700 text-default shadow-sm font-bold'
          : 'bg-transparent text-cortex-400 hover:text-cortex-200 hover:bg-cortex-800/30'}"
        onclick={() => {
          const wasSignalAnomaly = activeTab === 'signalAnomaly';
          activeTab = 'signalAnomaly';
          if (!wasSignalAnomaly) reloadSignalAnomalySegments();
        }}
      >
        {$t('validation.tab.signalAnomaly')}
      </button>
    </div>

    <div class="flex-1 overflow-y-auto p-4 space-y-4">
      {#if activeTab === 'dataset'}
        <ValidationDatasetTab
          {loading}
          {totalSegments}
          {passed}
          {errors}
          {warnings}
          bind:showErrors
          bind:showWarnings
          onJump={jumpToSegment}
        />
      {/if}

      {#if activeTab === 'active'}
        <ValidationActiveLearningTab
          loading={activeQueueLoading}
          queue={activeQueue}
          bind:targetError
          bind:confidenceLevel
          bind:queueLimit
          onJump={jumpToSegmentId}
        />
      {/if}

      {#if activeTab === 'signalAnomaly'}
        <ValidationSignalAnomalyTab
          running={signalAnomalyRunning}
          segments={signalAnomalySegments}
          bind:showAll={showAllSegmentsForSignalAnomaly}
          onJump={jumpToSegmentId}
        />
      {/if}
    </div>

    <footer class="flex justify-end gap-2 px-4 py-3 border-t border-cortex-800/50 shrink-0">
      {#if activeTab === 'dataset'}
        <button class="btn-secondary !text-xs" onclick={runValidation} disabled={loading}>
          {$t('validation.rerun')}
        </button>
      {:else if activeTab === 'active'}
        <button
          class="btn-secondary !text-xs"
          onclick={loadActiveLearningQueue}
          disabled={activeQueueLoading}
        >
          {$t('validation.activeLearning.run')}
        </button>
      {:else if activeTab === 'signalAnomaly'}
        <button
          class="btn-secondary !text-xs"
          onclick={runSignalAnomalyDetection}
          disabled={signalAnomalyRunning}
        >
          {#if signalAnomalyRunning}
            {$t('validation.signalAnomaly.running')}
          {:else}
            {$t('validation.signalAnomaly.run')}
          {/if}
        </button>
      {/if}
      <button class="btn-primary !text-xs" onclick={close}>{$t('close')}</button>
    </footer>
  </div>
</div>
