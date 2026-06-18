<script lang="ts">
  import { historyStore } from './stores/historyStore';
  import { onDestroy } from 'svelte';

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
      recordAction('Undone last action', 'edit');
    } catch (e) {
      console.error(e);
    }
  }

  async function handleRedo() {
    try {
      await historyStore.redo();
      recordAction('Redone action', 'edit');
    } catch (e) {
      console.error(e);
    }
  }
</script>

<div
  class="flex flex-col h-full bg-slate-950/40 backdrop-blur-md rounded-xl border border-slate-800/40 p-4 space-y-4"
>
  <!-- Undo/Redo Action Bar -->
  <div class="flex items-center justify-between border-b border-slate-800/60 pb-3">
    <h3 class="text-xs font-bold text-slate-400 uppercase tracking-widest">History Stack</h3>
    <div class="flex gap-1.5">
      <button
        class="px-3 py-1.5 rounded-lg border text-xs font-medium transition-all duration-150 flex items-center gap-1 relative
          {canUndo
          ? 'bg-indigo-600/20 text-indigo-300 border-indigo-500/30 hover:bg-indigo-600/30'
          : 'bg-transparent text-slate-600 border-slate-900 cursor-not-allowed opacity-40'}"
        onclick={handleUndo}
        disabled={!canUndo}
        title="Undo (Ctrl+Z)"
      >
        <svg class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M12.066 11.2a1 1 0 000 1.6l5.334 4A1 1 0 0019 16V8a1 1 0 00-1.6-.8l-5.334 4z"
          />
          <path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M4.066 11.2a1 1 0 000 1.6l5.334 4A1 1 0 0011 16V8a1 1 0 00-1.6-.8l-5.334 4z"
          />
        </svg>
        <span>Undo</span>
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
          : 'bg-transparent text-slate-600 border-slate-900 cursor-not-allowed opacity-40'}"
        onclick={handleRedo}
        disabled={!canRedo}
        title="Redo (Ctrl+Shift+Z)"
      >
        <span>Redo</span>
        <svg class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M11.934 12.8a1 1 0 000-1.6l-5.334-4A1 1 0 005 8v8a1 1 0 001.6.8l5.334-4z"
          />
          <path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M19.934 12.8a1 1 0 000-1.6l-5.334-4A1 1 0 0013 8v8a1 1 0 001.6.8l5.334-4z"
          />
        </svg>
        {#if showHotkeyOverlay && canRedo}
          <span
            class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
            >^+Z</span
          >
        {/if}
      </button>
    </div>
  </div>

  <!-- Session Edit Log -->
  <div
    class="flex-1 overflow-y-auto space-y-2 pe-1"
    style="scrollbar-width: thin; scrollbar-color: #475569 transparent;"
  >
    {#if localEdits.length === 0}
      <div class="flex flex-col items-center justify-center h-32 text-slate-600 text-xs italic">
        <span>No edits in this session</span>
      </div>
    {:else}
      <div class="space-y-1.5">
        {#each localEdits as log}
          <div
            class="flex items-start gap-2.5 p-2 rounded-lg bg-slate-900/30 border border-slate-900/60 hover:bg-slate-900/50 transition-colors"
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
              <p class="text-[11px] font-mono text-slate-300 leading-normal break-words">
                {log.description}
              </p>
              <span class="text-[9px] font-mono text-slate-500">{log.time}</span>
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>
