<script lang="ts">
  import type { AgentImportReport, AgentStageEvent } from './commands';
  import ErrorBoundary from './ErrorBoundary.svelte';
  import type { HistoryRecorder } from './historyAction';
  import { t } from './i18n';
  import LazyComponent from './LazyComponent.svelte';
  import PanelSplitter from './PanelSplitter.svelte';

  let {
    statsOpen,
    statsWidth = $bindable(),
    showHotkeyOverlay,
    latestAgentReport,
    latestAgentStageEvents,
    historyPanel = $bindable(),
    lazyLabels,
    loadAgentReportPanel,
    loadStatsDashboard,
  }: {
    statsOpen: boolean;
    statsWidth: number;
    showHotkeyOverlay: boolean;
    latestAgentReport: AgentImportReport | null;
    latestAgentStageEvents: AgentStageEvent[];
    historyPanel: HistoryRecorder | null;
    lazyLabels: {
      loadingLabel: string;
      failedLabel: string;
      retryLabel: string;
      closeLabel: string;
    };
    loadAgentReportPanel: () => Promise<unknown>;
    loadStatsDashboard: () => Promise<unknown>;
  } = $props();

  let historyLoadAttempt = $state(0);
  const historyPanelModule = $derived.by(() => {
    void historyLoadAttempt;
    return import('./HistoryPanel.svelte');
  });
</script>

<PanelSplitter
  direction="horizontal"
  label={$t('resizeStatsPanel')}
  value={statsWidth}
  onResize={(delta) => (statsWidth = Math.max(200, Math.min(600, statsWidth - delta)))}
/>
<ErrorBoundary>
  <aside
    data-testid="right-panel"
    class="shrink-0 flex flex-col border-l border-cortex-800/30 bg-cortex-900/40 backdrop-blur-md transition-all duration-200 overflow-hidden"
    class:panel-collapsed={!statsOpen}
    style="width: {statsWidth}px;"
  >
    {#if statsOpen}
      <div
        class="p-2 flex flex-col gap-3 h-full overflow-y-auto"
        role="region"
        aria-label={$t('stats.title')}
        style="scrollbar-width: thin; scrollbar-color: #475569 transparent;"
      >
        <LazyComponent
          load={loadAgentReportPanel}
          componentProps={{ report: latestAgentReport, stageEvents: latestAgentStageEvents }}
          {...lazyLabels}
        />
        <LazyComponent load={loadStatsDashboard} {...lazyLabels} />
        {#await historyPanelModule}
          <p role="status" aria-live="polite" class="text-sm text-cortex-300">
            {lazyLabels.loadingLabel}
          </p>
        {:then historyModule}
          {@const HistoryPanel = historyModule.default}
          <HistoryPanel bind:this={historyPanel} {showHotkeyOverlay} />
        {:catch cause}
          <div class="card space-y-3 p-4 text-center">
            <p role="alert" class="text-sm text-red-300">{lazyLabels.failedLabel}</p>
            <button
              type="button"
              class="btn btn-primary"
              onclick={() => {
                console.error('History panel load failed', cause);
                historyLoadAttempt += 1;
              }}>{lazyLabels.retryLabel}</button
            >
          </div>
        {/await}
      </div>
    {/if}
  </aside>
</ErrorBoundary>
