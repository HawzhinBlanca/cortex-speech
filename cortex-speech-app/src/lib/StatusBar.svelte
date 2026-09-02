<script lang="ts">
  import LoaderCircle from '@lucide/svelte/icons/loader-circle';
  import { cancelOperation } from './commands';
  import { t } from './i18n';
  import { modKeyLabel } from './keyboard';
  import { segmentStats } from './stores/segmentStore';
  import {
    agentPipelineStages,
    batchProgress,
    filesProcessed,
    isProcessing,
    pipelineCurrentFile,
    pipelinePhase,
    pipelineStatus,
    pipelineTotal,
    showKeyboardHelp,
    statusMessage,
  } from './stores/uiStore';

  const modKey = modKeyLabel();

  function agentStageTone(status: string): string {
    if (status === 'completed') return 'border-emerald-700/40 text-emerald-300 bg-emerald-950/30';
    if (status === 'blocked') return 'border-red-700/40 text-red-300 bg-red-950/30';
    return 'border-amber-700/40 text-amber-300 bg-amber-950/30';
  }

  function compactStageLabel(stage: string): string {
    return stage.replaceAll('_', ' ');
  }

  function fmtDuration(ms: number): string {
    const minutes = Math.floor(ms / 60_000);
    const seconds = Math.floor((ms % 60_000) / 1_000);
    return `${minutes}:${seconds.toString().padStart(2, '0')}`;
  }
</script>

<footer
  data-testid="status-bar"
  class="flex items-center justify-between px-4 py-1 glass border-t border-line shrink-0"
>
  <div class="flex items-center gap-3 text-[10px] text-cortex-500">
    <span>{$statusMessage}</span>
    {#if $isProcessing}
      <span class="flex items-center gap-1">
        <span class="w-1.5 h-1.5 rounded-full bg-amber-400 animate-pulse"></span>
        {$t('processing')}
      </span>
    {/if}
    {#if $pipelinePhase === 'importing'}
      <span data-testid="pipeline-import-status" class="flex items-center gap-2 text-amber-400">
        <LoaderCircle class="h-3 w-3 shrink-0 animate-spin" aria-hidden="true" />
        <span class="flex flex-col gap-0.5 min-w-0">
          <span
            >{$t('pipeline.importing')}
            {$t('pipeline.filesProcessed', {
              current: String($filesProcessed),
              total: String($pipelineTotal || '?'),
            })}</span
          >
          {#if $pipelineCurrentFile}
            <span class="text-cortex-500 truncate max-w-xs" title={$pipelineCurrentFile}>
              {$t('pipeline.currentFile', {
                file: $pipelineCurrentFile.split(/[/\\]/).pop()!,
              })}
            </span>
          {/if}
          {#if $pipelineStatus}
            <span class="text-cortex-600">{$t('pipeline.phase', { phase: $pipelineStatus })}</span>
          {/if}
        </span>
      </span>
      <button
        class="text-red-400 hover:text-red-300 px-1.5 py-0.5 border border-red-500/30 rounded shrink-0"
        onclick={cancelOperation}>{$t('pipeline.cancel')}</button
      >
    {:else if $pipelinePhase === 'reference_transcribing'}
      <span data-testid="pipeline-reference-status" class="flex items-center gap-2 text-amber-400">
        <LoaderCircle class="h-3 w-3 shrink-0 animate-spin" aria-hidden="true" />
        <span class="flex flex-col gap-0.5 min-w-0">
          <span>{$t('pipeline.referenceTranscribing')}</span>
          {#if $pipelineCurrentFile}
            <span class="text-cortex-500 truncate max-w-xs" title={$pipelineCurrentFile}>
              {$t('pipeline.currentFile', {
                file: $pipelineCurrentFile.split(/[/\\]/).pop()!,
              })}
            </span>
          {/if}
          {#if $pipelineStatus}
            <span class="text-cortex-600">{$t('pipeline.phase', { phase: $pipelineStatus })}</span>
          {/if}
        </span>
      </span>
      <button
        class="text-red-400 hover:text-red-300 px-1.5 py-0.5 border border-red-500/30 rounded shrink-0"
        onclick={cancelOperation}>{$t('pipeline.cancel')}</button
      >
    {:else if $pipelinePhase === 'detecting'}
      <span class="flex items-center gap-1 text-amber-400">
        <LoaderCircle class="h-3 w-3 animate-spin" aria-hidden="true" />
        {$t('pipeline.detecting')}
      </span>
    {:else if $pipelinePhase === 'transcribing'}
      <span class="flex items-center gap-1 text-amber-400">
        <LoaderCircle class="h-3 w-3 animate-spin" aria-hidden="true" />
        {$pipelineStatus || $t('pipeline.transcribing')}
        {$filesProcessed || $batchProgress.completed}/{$pipelineTotal ||
          $batchProgress.total ||
          '?'}
      </span>
      <button
        class="text-red-400 hover:text-red-300 px-1.5 py-0.5 border border-red-500/30 rounded shrink-0"
        data-testid="cancel-batch-transcribe-btn"
        onclick={cancelOperation}>{$t('pipeline.cancel')}</button
      >
    {:else if $pipelinePhase === 'adjudicating'}
      <span class="flex items-center gap-1 text-amber-400">
        <LoaderCircle class="h-3 w-3 animate-spin" aria-hidden="true" />
        {$t('pipeline.adjudicating')}
      </span>
    {/if}
    {#if $agentPipelineStages.length}
      <div
        class="hidden xl:flex items-center gap-1 max-w-[48rem] overflow-hidden"
        data-testid="agent-pipeline-timeline"
      >
        {#each $agentPipelineStages.slice(-5) as stage}
          <span
            class={`px-1.5 py-0.5 rounded border font-mono truncate max-w-[11rem] ${agentStageTone(stage.status)}`}
            title={`${compactStageLabel(stage.stage)}: ${stage.detail}`}
          >
            {compactStageLabel(stage.stage)}:{stage.status}
          </span>
        {/each}
      </div>
    {/if}
    {#if $batchProgress.status === 'running' && $pipelinePhase === 'idle'}
      <div class="flex items-center gap-2">
        <div class="w-20 h-1 bg-cortex-700 rounded-full overflow-hidden">
          <div
            class="h-full bg-cortex-400 rounded-full transition-all"
            style="width: {$batchProgress.percent}%"
          ></div>
        </div>
        <span
          >{$t('batchVerify.status', {
            current: String($batchProgress.completed),
            total: String($batchProgress.total),
          })}</span
        >
        <button
          class="text-red-400 hover:text-red-300 px-1.5 py-0.5 border border-red-500/30 rounded"
          onclick={cancelOperation}>{$t('pipeline.cancel')}</button
        >
      </div>
    {/if}
  </div>
  <div class="flex items-center gap-3 text-[10px] text-cortex-500">
    <span>{$segmentStats.total} {$t('segments')}</span>
    <span>{fmtDuration($segmentStats.totalDurationMs)} {$t('total')}</span>
    <span>{$segmentStats.verified}/{$segmentStats.total} {$t('verifiedCount')}</span>
    <span class="text-[10px] text-cortex-500">{$t('pressForShortcuts')}</span>
    <button
      class="hover:text-cortex-400 transition-colors"
      onclick={() => showKeyboardHelp.set(true)}
      title="{modKey}+/"
      aria-label={$t('keyboardShortcuts')}
    >
      <kbd class="text-[9px]">{modKey}+/</kbd>
      {$t('shortcuts')}
    </button>
  </div>
</footer>
