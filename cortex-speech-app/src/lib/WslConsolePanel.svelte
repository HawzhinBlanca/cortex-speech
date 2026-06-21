<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { focusTrap } from './actions/focusTrap';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import * as api from './commands';
  import { showWslConsole } from './stores/uiStore';
  import { notifications } from './stores/notificationStore';
  import { appendBoundedLogLine } from './logBuffer';
  import { t } from './i18n';

  let running = $state(false);
  let status = $state<'idle' | 'running' | 'completed' | 'failed' | 'cancelled'>('idle');
  let exitCode = $state<number | null>(null);

  // Options
  let limitFiles = $state<number | undefined>(undefined);
  let limitSegments = $state<number | undefined>(undefined);
  let dryRun = $state(false);
  let testOne = $state(false);

  // Console Logs
  let logs = $state<string[]>([]);
  let consoleContainer = $state<HTMLDivElement | null>(null);

  let unlistenLog: UnlistenFn | null = null;
  let unlistenStatus: UnlistenFn | null = null;

  function appendLog(line: string) {
    logs = appendBoundedLogLine(logs, line);
    // Auto scroll to bottom
    if (consoleContainer) {
      setTimeout(() => {
        if (consoleContainer) {
          consoleContainer.scrollTop = consoleContainer.scrollHeight;
        }
      }, 30);
    }
  }

  async function startRefinement() {
    if (running) return;

    running = true;
    status = 'running';
    exitCode = null;
    logs = [];
    appendLog('>>> Spawning configured external ASR provider via WSL...');
    appendLog(
      `>>> Command: wsl /root/cortex_env/bin/python3 <configured-provider-script>` +
        (limitFiles ? ` --limit-files ${limitFiles}` : '') +
        (limitSegments ? ` --limit-segments ${limitSegments}` : '') +
        (dryRun ? ' --dry-run' : '') +
        (testOne ? ' --test-one' : ''),
    );

    try {
      await api.runWslRefinement({
        limit_files: limitFiles,
        limit_segments: limitSegments,
        dry_run: dryRun,
        test_one: testOne,
      });
    } catch (e) {
      appendLog(`[SYSTEM ERROR] Failed to start refinement: ${e}`);
      status = 'failed';
      running = false;
      notifications.error($t('wsl.startFailed'), { detail: String(e) });
    }
  }

  async function stopRefinement() {
    if (!running) return;
    appendLog('\n>>> Aborting process by user request...');
    try {
      await api.cancelWslRefinement();
      status = 'cancelled';
    } catch (e) {
      appendLog(`[SYSTEM ERROR] Abort failed: ${e}`);
      notifications.error($t('wsl.cancelFailed'), { detail: String(e) });
    }
  }

  function clearLogs() {
    logs = [];
  }

  function copyLogs() {
    const text = logs.join('\n');
    navigator.clipboard.writeText(text);
    notifications.success($t('wsl.logsCopied'));
  }

  function close() {
    if (running) {
      notifications.warning($t('wsl.stillRunning'));
      return;
    }
    showWslConsole.set(false);
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') close();
  }

  onMount(async () => {
    // Listen to log events from Rust subprocess
    unlistenLog = await listen<string>('wsl-log', (event) => {
      appendLog(event.payload);
    });

    // Listen to exit status from Rust subprocess
    unlistenStatus = await listen<{
      status: 'completed' | 'failed' | 'cancelled';
      exit_code: number;
    }>('wsl-status', (event) => {
      // In-panel display only. The completion side effects (toast + segment refresh) are handled
      // app-scoped in events.ts so they fire even when this panel is closed mid-run.
      status = event.payload.status;
      exitCode = event.payload.exit_code;
      running = false;
    });
  });

  onDestroy(() => {
    try {
      if (unlistenLog) unlistenLog();
    } catch (e) {
      // ignore
    }
    try {
      if (unlistenStatus) unlistenStatus();
    } catch (e) {
      // ignore
    }
  });
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  class="fixed inset-0 z-50 flex items-center justify-center bg-black/75 backdrop-blur-md"
  role="dialog"
  aria-modal="true"
  tabindex="-1"
  use:focusTrap
  onkeydown={handleKeydown}
  onclick={(e) => {
    if (e.target === e.currentTarget) close();
  }}
>
  <div
    class="card p-0 max-w-3xl w-full mx-4 max-h-[85vh] flex flex-col shadow-2xl border border-cortex-800/40 bg-cortex-950/90 text-default"
  >
    <!-- Header -->
    <div class="flex items-center justify-between px-6 py-4 border-b border-cortex-800/50">
      <div class="flex items-center gap-3">
        <svg class="w-5 h-5 text-cortex-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M8 9l3 3-3 3m5 0h3M5 20h14a2 2 0 002-2V6a2 2 0 00-2-2H5a2 2 0 00-2 2v12a2 2 0 002 2z"
          />
        </svg>
        <h2 class="text-md font-semibold text-default">
          Meta OmniASR 7B v2 Local Transcription (WSL)
        </h2>
      </div>
      <button
        class="text-muted hover:text-default transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
        onclick={close}
        disabled={running}
        aria-label="Close"
      >
        ✕
      </button>
    </div>

    <!-- Body -->
    <div class="flex-1 overflow-y-auto p-6 space-y-4 min-h-0 flex flex-col">
      <!-- Description & Config -->
      <div class="bg-cortex-900/50 border border-cortex-800/40 rounded-xl p-4 space-y-3 shrink-0">
        <p class="text-xs text-cortex-300">
          Run high-accuracy offline transcription on the GPU using Meta's 7B encoder-decoder model.
          This process executes inside WSL (Ubuntu) and updates segment transcripts directly in the
          database.
        </p>

        <div class="grid grid-cols-2 gap-4">
          <label class="flex flex-col gap-1 text-xs text-muted">
            <span>{$t('wsl.limitFiles')}</span>
            <input
              type="number"
              min="1"
              bind:value={limitFiles}
              placeholder="e.g., 5"
              class="input !py-1"
              disabled={running}
            />
          </label>
          <label class="flex flex-col gap-1 text-xs text-muted">
            <span>{$t('wsl.limitSegments')}</span>
            <input
              type="number"
              min="1"
              bind:value={limitSegments}
              placeholder="e.g., 20"
              class="input !py-1"
              disabled={running}
            />
          </label>
        </div>

        <div class="flex gap-6 pt-1">
          <label class="flex items-center gap-2 cursor-pointer text-xs text-muted">
            <input
              type="checkbox"
              bind:checked={dryRun}
              class="accent-cortex-500"
              disabled={running}
            />
            <span>{$t('wsl.dryRun')}</span>
          </label>

          <label class="flex items-center gap-2 cursor-pointer text-xs text-muted">
            <input
              type="checkbox"
              bind:checked={testOne}
              class="accent-cortex-500"
              disabled={running}
            />
            <span>{$t('wsl.testMode')}</span>
          </label>
        </div>
      </div>

      <!-- Log Terminal Console -->
      <div class="flex-1 min-h-[250px] flex flex-col min-h-0">
        <div class="flex items-center justify-between text-xs text-cortex-400 mb-1 px-1">
          <span>{$t('wsl.terminalLogs')}</span>
          <div class="flex items-center gap-2">
            {#if status === 'running'}
              <span class="flex items-center gap-1.5 text-cyan-400 font-semibold">
                <svg class="animate-spin h-3.5 w-3.5" fill="none" viewBox="0 0 24 24">
                  <circle
                    class="opacity-25"
                    cx="12"
                    cy="12"
                    r="10"
                    stroke="currentColor"
                    stroke-width="4"
                  />
                  <path
                    class="opacity-75"
                    fill="currentColor"
                    d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
                  />
                </svg>
                {$t('wsl.processing')}
              </span>
            {:else if status === 'completed'}
              <span class="text-emerald-400 font-semibold">● {$t('wsl.completed')}</span>
            {:else if status === 'failed'}
              <span class="text-red-400 font-semibold">● {$t('wsl.failed')}</span>
            {:else if status === 'cancelled'}
              <span class="text-amber-400 font-semibold">● {$t('wsl.cancelled')}</span>
            {:else}
              <span class="text-cortex-500">{$t('wsl.idle')}</span>
            {/if}
          </div>
        </div>

        <!-- Monospace Log Container -->
        <div
          bind:this={consoleContainer}
          class="terminal-dark flex-1 overflow-y-auto bg-black font-mono text-[11px] p-4 rounded-xl border border-cortex-800/60 space-y-1 select-text scrollbar-thin scrollbar-thumb-cortex-800 scrollbar-track-transparent min-h-0"
        >
          {#if logs.length === 0}
            <div class="text-cortex-600 italic">
              {$t('wsl.noLogs')}
            </div>
          {:else}
            {#each logs as log}
              {#if log.startsWith('>>>')}
                <div class="text-cyan-400 font-semibold select-all">{log}</div>
              {:else if log.includes('[ERROR]') || log.includes('[SYSTEM ERROR]')}
                <div class="text-red-400 font-semibold select-all">{log}</div>
              {:else if log.includes('loaded successfully') || log.includes('Complete!')}
                <div class="text-emerald-400 select-all">{log}</div>
              {:else}
                <div class="text-muted select-all">{log}</div>
              {/if}
            {/each}
          {/if}
        </div>
      </div>
    </div>

    <!-- Footer -->
    <div
      class="flex items-center justify-between px-6 py-4 border-t border-cortex-800/50 bg-cortex-900/20 rounded-b-2xl"
    >
      <div class="flex gap-2">
        <button
          class="btn btn-secondary !text-xs disabled:opacity-30"
          onclick={clearLogs}
          disabled={logs.length === 0}
        >
          {$t('wsl.clearLogs')}
        </button>
        <button
          class="btn btn-secondary !text-xs disabled:opacity-30"
          onclick={copyLogs}
          disabled={logs.length === 0}
        >
          {$t('wsl.copyLogs')}
        </button>
      </div>

      <div class="flex gap-3">
        <button class="btn btn-secondary !text-xs" onclick={close} disabled={running}>
          {$t('close')}
        </button>
        {#if running}
          <button
            class="btn btn-primary !bg-red-600 hover:!bg-red-500 !border-red-700 !text-xs"
            onclick={stopRefinement}
          >
            {$t('wsl.cancelStop')}
          </button>
        {:else}
          <button class="btn btn-primary !text-xs" onclick={startRefinement}>
            {$t('wsl.startBatch')}
          </button>
        {/if}
      </div>
    </div>
  </div>
</div>
