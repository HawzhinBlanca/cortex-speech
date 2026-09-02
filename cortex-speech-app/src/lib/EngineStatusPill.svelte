<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as api from './commands';
  import { t } from './i18n';
  import { notifications } from './stores/notificationStore';

  // Always-visible signal of whether the champion (OmniASR-7B) engine is up, so the owner never has
  // to discover it's down only when an import fails. 'starting' is a UI-derived state: once the owner
  // clicks Start (or launches it), an offline poll is treated as still-warming (~30 GB, 1-5 min) until
  // it reports ready.
  type EngineState = 'checking' | 'ready' | 'offline' | 'starting';
  let state = $state<EngineState>('checking');
  let poll: ReturnType<typeof setInterval> | undefined;
  // 'starting' must not be a one-way state: if the engine process dies during its multi-minute
  // model load, polls kept 'starting' alive forever and the Start button (offline-only) never came
  // back. Give the warmup a generous deadline, then honestly return to offline.
  const WARMUP_DEADLINE_MS = 8 * 60 * 1000;
  let startingSince = 0;

  async function refresh() {
    const warmupExpired = state === 'starting' && Date.now() - startingSince > WARMUP_DEADLINE_MS;
    try {
      const s = await api.getChampionEngineStatus();
      if (s.ready) state = 'ready';
      else if (state !== 'starting' || warmupExpired) state = 'offline';
    } catch {
      if (state !== 'starting' || warmupExpired) state = 'offline';
    }
  }

  async function start() {
    try {
      state = 'starting';
      startingSince = Date.now();
      await api.startChampionEngine();
    } catch (e) {
      state = 'offline';
      notifications.error($t('engine.startFailed'), { cause: e });
    }
  }

  onMount(() => {
    void refresh();
    poll = setInterval(refresh, 15000); // light background poll so a server that comes up is noticed
  });
  onDestroy(() => {
    if (poll) clearInterval(poll);
  });

  const dot = $derived(
    state === 'ready'
      ? 'bg-green-400'
      : state === 'starting' || state === 'checking'
        ? 'bg-amber-400 animate-pulse'
        : 'bg-red-400',
  );
</script>

<div
  class="flex items-center gap-1.5 text-[11px]"
  data-testid="engine-status"
  title={$t('engine.tooltip')}
>
  <span class="inline-block w-2 h-2 rounded-full {dot}" aria-hidden="true"></span>
  <span class="text-cortex-300">{$t(`engine.${state}`)}</span>
  {#if state === 'offline'}
    <button
      type="button"
      class="btn btn-secondary !text-[10px] !py-0.5 !px-1.5 ms-1"
      onclick={start}
      data-testid="engine-start-btn"
    >
      {$t('engine.start')}
    </button>
  {/if}
</div>
