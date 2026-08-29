<script lang="ts">
  import { activeOperations } from './invoke';
  import { t } from './i18n';
  import {
    batchProgress,
    filesProcessed,
    isProcessing,
    pipelinePhase,
    pipelineTotal,
  } from './stores/uiStore';

  const percent = $derived(
    $batchProgress.total > 0
      ? $batchProgress.percent
      : $pipelineTotal > 0
        ? Math.round(($filesProcessed / $pipelineTotal) * 100)
        : -1,
  );
  const detailedProgressActive = $derived(
    $pipelinePhase !== 'idle' || $batchProgress.status === 'running' || $isProcessing,
  );
  let progressLoadAttempt = $state(0);
  const processingProgressModule = $derived.by(() => {
    void progressLoadAttempt;
    return import('./ProcessingProgress.svelte');
  });
</script>

{#if $activeOperations.size > 0}
  <div
    class="h-0.5 shrink-0 overflow-hidden bg-accent-soft"
    role="progressbar"
    aria-valuemin="0"
    aria-valuemax="100"
    aria-valuenow={percent >= 0 ? percent : undefined}
  >
    {#if percent >= 0}
      <div
        class="h-full rounded-full bg-accent transition-[width] duration-300 ease-smooth"
        style="width: {Math.min(100, Math.max(2, percent))}%"
      ></div>
    {:else}
      <div class="h-full w-2/5 rounded-full bg-accent animate-progress-indeterminate"></div>
    {/if}
  </div>
{/if}

{#if detailedProgressActive}
  {#await processingProgressModule then progressModule}
    {@const ProcessingProgress = progressModule.default}
    <ProcessingProgress />
  {:catch cause}
    <button
      type="button"
      class="btn btn-secondary m-2"
      onclick={() => {
        console.error('Processing progress load failed', cause);
        progressLoadAttempt += 1;
      }}>{$t('retry')}</button
    >
  {/await}
{/if}
