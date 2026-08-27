<script lang="ts">
  import ChevronDown from '@lucide/svelte/icons/chevron-down';
  import * as api from './commands';
  import { computeLocalDiff } from './diff/compute';
  import type { DiffResult } from './diff/types';
  import { t } from './i18n';
  import { isTauriRuntime } from './runtime';
  import { formatPublicErrorReference } from './errorText';

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

    // Desktop refusals are authoritative. Falling back after `DIFF_TOO_LARGE`/`DIFF_TOO_COMPLEX`
    // would repeat the same memory-heavy work in the renderer and previously turned a refusal into
    // a fabricated 100% similarity result. Browser preview uses the bounded local implementation.
    const compute = isTauriRuntime()
      ? api.computeDiff(source, target)
      : Promise.resolve().then(() => computeLocalDiff(source, target));

    compute
      .then((result) => {
        if (!cancelled) diff = result as DiffResult;
      })
      .catch((e) => {
        if (!cancelled) {
          diff = null;
          error = formatPublicErrorReference(e) ?? $t('errors.unknown');
        }
      })
      .finally(() => {
        if (!cancelled) loading = false;
      });

    return () => {
      cancelled = true;
    };
  });

  function opClass(op: string): string {
    switch (op) {
      case 'Insert':
        return 'bg-emerald-900/50 text-emerald-200 rounded px-0.5';
      case 'Delete':
        return 'bg-red-900/50 text-red-300 line-through rounded px-0.5';
      case 'Replace':
        return 'bg-amber-900/50 text-amber-200 rounded px-0.5';
      default:
        return 'text-cortex-200';
    }
  }
</script>

{#if raw?.trim() && annotated?.trim() && raw.trim() !== annotated.trim()}
  <div class="rounded-lg border border-cortex-800/50 bg-cortex-950/40 overflow-hidden">
    <button
      type="button"
      class="w-full flex items-center justify-between px-3 py-2 text-start hover:bg-cortex-800/30 transition-colors"
      onclick={() => (collapsed = !collapsed)}
      aria-expanded={!collapsed}
    >
      <span class="text-[11px] font-semibold text-cortex-300 uppercase tracking-wider"
        >{$t('diff.title')}</span
      >
      <span class="text-[10px] text-cortex-500 flex items-center gap-2">
        {#if loading}
          {$t('loading')}
        {:else if diff}
          {$t('diff.similarity', { pct: diff.stats.similarity.toFixed(0) })}
        {/if}
        <ChevronDown
          class="w-3 h-3 transition-transform {collapsed ? '' : 'rotate-180'}"
          aria-hidden="true"
        />
      </span>
    </button>

    {#if !collapsed}
      <div class="px-3 pb-3 space-y-2">
        {#if error}
          <p class="text-xs text-red-400">{$t('diff.error')}: {error}</p>
        {:else if loading}
          <div class="h-8 bg-cortex-800/30 rounded animate-pulse"></div>
        {:else if diff}
          <!-- dir="rtl": these are Kurdish (RTL) word chips in logical order; a flex row reverses
               them unless the base direction is RTL (matching App.svelte's word-chip rows). <bdi>
               isolates each chip so an embedded LTR token (or the Replace "raw → ann" arrow) can't
               reorder the row or swap the two words within a chip. -->
          <div
            class="flex flex-wrap gap-x-1 gap-y-1 text-sm font-mono leading-relaxed"
            dir="rtl"
            lang="ckb"
          >
            {#each diff.changes as change}
              <span class={opClass(change.op)}><bdi>{change.value}</bdi></span>
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
