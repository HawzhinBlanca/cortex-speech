<script lang="ts">
  import { onMount } from 'svelte';
  import {
    getTracingStats,
    getRecentSpans,
    clearTracingSpans,
    type TracingStats,
    type TracingSpan,
  } from './commands';
  import { notifications } from './stores/notificationStore';
  import { isTauriRuntime } from './runtime';

  // Developer diagnostics: aggregate operation-timing stats and the most recent spans recorded by
  // the in-process Tracer. Read-only except for the Clear action; never exposes audio or transcripts.
  let stats = $state<TracingStats | null>(null);
  let spans = $state<TracingSpan[]>([]);
  let loading = $state(true);
  const tauriAvailable = isTauriRuntime();

  async function load() {
    if (!tauriAvailable) {
      loading = false;
      return;
    }
    loading = true;
    try {
      [stats, spans] = await Promise.all([getTracingStats(), getRecentSpans(50)]);
    } catch (e: unknown) {
      notifications.error('Failed to load diagnostics', { detail: String(e) });
    } finally {
      loading = false;
    }
  }

  async function clear() {
    if (!tauriAvailable) return;
    try {
      await clearTracingSpans();
      await load();
    } catch (e: unknown) {
      notifications.error('Failed to clear diagnostics', { detail: String(e) });
    }
  }

  onMount(load);

  function ms(n: number): string {
    return `${n.toFixed(1)} ms`;
  }
</script>

<section class="space-y-3" data-testid="diagnostics-panel">
  <div class="flex items-center justify-between">
    <h3 class="text-sm font-medium text-cortex-100">Diagnostics — operation tracing</h3>
    <div class="flex gap-2">
      <button class="btn-ghost text-xs" onclick={load} data-testid="diagnostics-refresh"
        >Refresh</button
      >
      <button class="btn-ghost text-xs" onclick={clear} data-testid="diagnostics-clear"
        >Clear</button
      >
    </div>
  </div>

  {#if !tauriAvailable}
    <p class="text-xs text-muted">Diagnostics require the desktop runtime.</p>
  {:else if loading}
    <p class="text-xs text-muted">Loading…</p>
  {:else}
    {#if stats}
      <div class="grid grid-cols-2 gap-2" data-testid="diagnostics-stats">
        <div class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-2">
          <div class="text-lg font-semibold text-cortex-100">{stats.total_spans}</div>
          <div class="text-[10px] text-muted">spans recorded</div>
        </div>
        <div class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-2">
          <div
            class="text-lg font-semibold {stats.failures > 0
              ? 'text-amber-300'
              : 'text-cortex-100'}"
          >
            {stats.failures}
          </div>
          <div class="text-[10px] text-muted">failures</div>
        </div>
        <div class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-2">
          <div class="text-lg font-semibold text-cortex-100">{ms(stats.avg_duration_ms)}</div>
          <div class="text-[10px] text-muted">avg duration</div>
        </div>
        <div class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-2">
          <div class="text-lg font-semibold text-cortex-100">{ms(stats.total_duration_ms)}</div>
          <div class="text-[10px] text-muted">total duration</div>
        </div>
      </div>
    {/if}

    {#if spans.length === 0}
      <p class="text-xs text-muted" data-testid="diagnostics-empty">
        No operations have been traced yet. Run a transcription or import to populate timings.
      </p>
    {:else}
      <ul class="space-y-1" data-testid="diagnostics-spans">
        {#each spans as s, i (i)}
          <li
            class="flex items-center justify-between rounded border border-cortex-700/30 px-2 py-1 text-xs"
          >
            <span class="flex items-center gap-2">
              <span aria-hidden="true">{s.success ? '✓' : '✕'}</span>
              <span class="text-cortex-100">{s.operation}</span>
              {#if !s.success}<span class="text-amber-300">(failed)</span>{/if}
            </span>
            <span class="text-muted">{ms(s.duration_ms)}</span>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
</section>
