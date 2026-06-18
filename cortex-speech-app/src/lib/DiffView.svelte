<script lang="ts">
  import * as api from './commands';
  import { computeLocalDiff } from './diff/compute';
  import type { DiffResult } from './diff/types';
  import { t } from './i18n';
  import { isTauriRuntime } from './runtime';

  interface Props {
    raw: string;
    annotated: string;
  }

  let { raw, annotated }: Props = $props();

  let diff = $state<DiffResult | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let collapsed = $state(false);

  $effect(() => {
    const source = raw?.trim();
    const target = annotated?.trim();
    if (!source || !target || source === target) {
      diff = null;
      error = null;
      loading = false;
      return;
    }

    let cancelled = false;
    loading = true;
    error = null;

    const compute = isTauriRuntime()
      ? api.computeDiff(source, target).catch(() => computeLocalDiff(source, target))
      : Promise.resolve(computeLocalDiff(source, target));

    compute
      .then((result) => {
        if (!cancelled) diff = result as DiffResult;
      })
      .catch((e) => {
        if (!cancelled) {
          diff = null;
          error = String(e);
        }
      })
      .finally(() => {
        if (!cancelled) loading = false;
      });

    return () => { cancelled = true; };
  });

  function opClass(op: string): string {
    switch (op) {
      case 'Insert': return 'bg-emerald-900/50 text-emerald-200 rounded px-0.5';
      case 'Delete': return 'bg-red-900/50 text-red-300 line-through rounded px-0.5';
      case 'Replace': return 'bg-amber-900/50 text-amber-200 rounded px-0.5';
      default: return 'text-cortex-200';
    }
  }
</script>

{#if raw?.trim() && annotated?.trim() && raw.trim() !== annotated.trim()}
  <div class="rounded-lg border border-cortex-800/50 bg-cortex-950/40 overflow-hidden">
    <button
      type="button"
      class="w-full flex items-center justify-between px-3 py-2 text-start hover:bg-cortex-800/30 transition-colors"
      onclick={() => collapsed = !collapsed}
      aria-expanded={!collapsed}
    >
      <span class="text-[11px] font-semibold text-cortex-300 uppercase tracking-wider">{$t('diff.title')}</span>
      <span class="text-[10px] text-cortex-500 flex items-center gap-2">
        {#if loading}
          {$t('loading')}
        {:else if diff}
          {$t('diff.similarity', { pct: diff.stats.similarity.toFixed(0) })}
        {/if}
        <svg class="w-3 h-3 transition-transform {collapsed ? '' : 'rotate-180'}" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 9l-7 7-7-7"/>
        </svg>
      </span>
    </button>

    {#if !collapsed}
      <div class="px-3 pb-3 space-y-2">
        {#if error}
          <p class="text-xs text-red-400">{$t('diff.error')}: {error}</p>
        {:else if loading}
          <div class="h-8 bg-cortex-800/30 rounded animate-pulse"></div>
        {:else if diff}
          <div class="flex flex-wrap gap-x-1 gap-y-1 text-sm font-mono leading-relaxed" dir="auto">
            {#each diff.changes as change}
              <span class={opClass(change.op)}>{change.value}</span>
            {/each}
          </div>
          <div class="flex flex-wrap gap-3 text-[10px] text-cortex-500 pt-1">
            <span class="text-emerald-400">+{diff.stats.added_words} {$t('diff.added')}</span>
            <span class="text-red-400">−{diff.stats.removed_words} {$t('diff.removed')}</span>
            <span class="text-amber-400">~{diff.stats.changed_words} {$t('diff.changed')}</span>
          </div>
        {/if}
      </div>
    {/if}
  </div>
{/if}
