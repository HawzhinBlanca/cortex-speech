<script lang="ts">
  import { onDestroy } from 'svelte';
  import {
    searchQuery,
    searchResults,
    filterVerified,
    sortOrder,
    segments,
  } from './stores/segmentStore';
  import { t } from './i18n';
  import { isTauriRuntime } from './runtime';

  let query = $state('');
  let loading = $state(false);
  let debounceTimer: ReturnType<typeof setTimeout>;
  let searchGeneration = 0;
  const tauriAvailable = isTauriRuntime();
  // Mirror EXTERNAL changes to the shared search store (session restore on launch, or a programmatic
  // clear) into the local input. `lastPushed` tracks what this component itself wrote, so the user's
  // own debounced input is never echoed back and cannot fight their typing.
  let lastPushed = '';
  $effect(() => {
    const external = $searchQuery;
    if (external !== lastPushed) {
      query = external;
      lastPushed = external;
    }
  });

  // Cancel any pending debounce on unmount so a stale keystroke can't write the global search stores
  // after the component is gone (e.g. sidebar closed within the 250ms debounce).
  onDestroy(() => clearTimeout(debounceTimer));

  async function reloadView(gen: number) {
    loading = true;
    if (!tauriAvailable) {
      loading = false;
      return;
    }
    try {
      await segments.load();
    } finally {
      if (gen === searchGeneration) loading = false;
    }
  }

  function handleInput() {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
      const trimmed = query.trim();
      searchQuery.set(trimmed);
      lastPushed = trimmed;
      const gen = ++searchGeneration;
      searchResults.set(null);
      void reloadView(gen);
    }, 250);
  }

  function handleClear() {
    clearTimeout(debounceTimer);
    searchGeneration++;
    query = '';
    searchQuery.set('');
    lastPushed = '';
    searchResults.set(null);
    void reloadView(searchGeneration);
  }

  function setVerified(value: boolean | null) {
    filterVerified.set(value);
    void reloadView(++searchGeneration);
  }

  function setSort(value: string) {
    sortOrder.set(
      value as 'newest' | 'oldest' | 'duration' | 'verified' | 'confidence' | 'activeLearning',
    );
    void reloadView(++searchGeneration);
  }
</script>

<div class="space-y-2" data-testid="search-bar">
  <div class="relative">
    <input
      type="search"
      data-testid="search-input"
      class="input ps-3 pe-28"
      placeholder={$t('searchPlaceholder')}
      bind:value={query}
      oninput={handleInput}
      aria-busy={loading}
    />
    {#if query}
      <div class="absolute end-3 top-1/2 -translate-y-1/2 flex items-center gap-2">
        {#if loading}
          <span class="text-[10px] text-cortex-400" role="status">{$t('loading')}</span>
        {/if}
        <button
          type="button"
          class="text-xs text-cortex-500 hover:text-cortex-300"
          onclick={handleClear}
          aria-label={$t('clearSearch')}
        >
          {$t('clearSearch')}
        </button>
      </div>
    {/if}
  </div>

  <div class="flex items-center gap-2">
    <div class="flex gap-1">
      {#each [{ label: $t('all'), value: null }, { label: $t('filterVerified'), value: true }, { label: $t('filterPending'), value: false }] as opt}
        <button
          class="text-xs px-2 py-1 rounded transition-colors {$filterVerified === opt.value
            ? 'bg-cortex-700 text-cortex-100'
            : 'text-cortex-400 hover:text-cortex-200'}"
          onclick={() => setVerified(opt.value)}>{opt.label}</button
        >
      {/each}
    </div>
    <div class="ms-auto">
      <select
        class="text-xs bg-cortex-800 border border-cortex-700 rounded px-2 py-1 text-cortex-300"
        aria-label={$t('sortBy')}
        value={$sortOrder}
        oninput={(e) => setSort((e.target as HTMLSelectElement).value)}
      >
        <option value="newest">{$t('sortNewest')}</option>
        <option value="oldest">{$t('sortOldest')}</option>
        <option value="duration">{$t('sortDuration')}</option>
        <option value="verified">{$t('sortVerified')}</option>
        <option value="confidence">{$t('sortConfidence')}</option>
        <option value="activeLearning">{$t('sortActiveLearning')}</option>
      </select>
    </div>
  </div>
</div>
