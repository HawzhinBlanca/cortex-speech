<script lang="ts">
  import { historyStore } from './stores/historyStore';
  import { onDestroy } from 'svelte';
  import { t } from './i18n';

  // We can track local session edits
  interface EditRecord {
    id: string;
    description: string;
    time: string;
    type: 'edit' | 'verify' | 'delete' | 'import';
  }

  let { showHotkeyOverlay = false } = $props();
  let localEdits = $state<EditRecord[]>([]);
  let canUndo = $state(false);
  let canRedo = $state(false);

  const unsubscribe = historyStore.subscribe(($history) => {
    canUndo = $history.canUndo;
    canRedo = $history.canRedo;
  });

  onDestroy(() => {
    unsubscribe();
  });

  // Expose function to add local actions to the log
  export function recordAction(description: string, type: EditRecord['type']) {
    const time = new Date().toLocaleTimeString([], {
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    });
    localEdits = [
      { id: Math.random().toString(), description, time, type },
      ...localEdits.slice(0, 19), // limit to 20 actions
    ];
  }

  async function handleUndo() {
    try {
      await historyStore.undo();
      recordAction($t('history.undoneAction'), 'edit');
    } catch (e) {
      console.error(e);
    }
  }

  async function handleRedo() {
    try {
      await historyStore.redo();
      recordAction($t('history.redoneAction'), 'edit');
    } catch (e) {
      console.error(e);
    }
  }
</script>

<div
  class="flex flex-col h-full bg-cortex-950/40 backdrop-blur-md rounded-xl border border-cortex-800/40 p-4 space-y-4"
>
  <!-- Undo/Redo Action Bar -->
  <div class="flex items-center justify-between border-b border-cortex-800/60 pb-3">
    <h3 class="text-xs font-bold text-cortex-400 uppercase tracking-widest">
      {$t('history.stack')}
    </h3>
    <div class="flex gap-1.5">
      <button
        class="px-3 py-1.5 rounded-lg border text-xs font-medium transition-all duration-150 flex items-center gap-1 relative
          {canUndo
          ? 'bg-indigo-600/20 text-indigo-300 border-indigo-500/30 hover:bg-indigo-600/30'
          : 'bg-transparent text-cortex-600 border-cortex-900 cursor-not-allowed opacity-40'}"
        onclick={handleUndo}
        disabled={!canUndo}
        title={$t('history.undoShortcut')}
        aria-describedby={!canUndo ? 'history-no-undo' : undefined}
      >
        <span>{$t('undo')}</span>
        {#if showHotkeyOverlay && canUndo}
          <span
            class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
            >^Z</span
          >
        {/if}
      </button>

      <button
        class="px-3 py-1.5 rounded-lg border text-xs font-medium transition-all duration-150 flex items-center gap-1 relative
          {canRedo
          ? 'bg-indigo-600/20 text-indigo-300 border-indigo-500/30 hover:bg-indigo-600/30'
          : 'bg-transparent text-cortex-600 border-cortex-900 cursor-not-allowed opacity-40'}"
        onclick={handleRedo}
        disabled={!canRedo}
        title={$t('history.redoShortcut')}
        aria-describedby={!canRedo ? 'history-no-redo' : undefined}
      >
        <span>{$t('redo')}</span>
        {#if showHotkeyOverlay && canRedo}
          <span
            class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
            >^+Z</span
          >
        {/if}
      </button>
      <span id="history-no-undo" class="sr-only">{$t('history.nothingToUndo')}</span>
      <span id="history-no-redo" class="sr-only">{$t('history.nothingToRedo')}</span>
    </div>
  </div>

  <!-- Session Edit Log -->
  <div
    class="flex-1 overflow-y-auto space-y-2 pe-1"
    style="scrollbar-width: thin; scrollbar-color: #475569 transparent;"
  >
    {#if localEdits.length === 0}
      <div class="flex flex-col items-center justify-center h-32 text-cortex-600 text-xs italic">
        <span>{$t('history.noEdits')}</span>
      </div>
    {:else}
      <div class="space-y-1.5">
        {#each localEdits as log}
          <div
            class="flex items-start gap-2.5 p-2 rounded-lg bg-cortex-900/30 border border-cortex-900/60 hover:bg-cortex-900/50 transition-colors"
          >
            <!-- Mini icon for type -->
            <span
              class="mt-0.5 w-1.5 h-1.5 rounded-full shrink-0
              {log.type === 'verify'
                ? 'bg-emerald-500'
                : log.type === 'delete'
                  ? 'bg-red-500'
                  : log.type === 'import'
                    ? 'bg-blue-500'
                    : 'bg-indigo-500'}"
            >
            </span>
            <div class="flex-1 min-w-0">
              <p class="text-[11px] font-mono text-cortex-300 leading-normal break-words">
                {log.description}
              </p>
              <span class="text-[9px] font-mono text-cortex-500">{log.time}</span>
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>
