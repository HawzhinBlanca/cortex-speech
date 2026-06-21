<script lang="ts">
  import { onDestroy } from 'svelte';
  import * as api from './commands';
  import { notifications } from './stores/notificationStore';
  import {
    searchQuery,
    searchResults,
    searchLoading,
    filterVerified,
    sortOrder,
  } from './stores/segmentStore';
  import { t } from './i18n';
  import { isTauriRuntime } from './runtime';

  let query = $state('');
  let debounceTimer: ReturnType<typeof setTimeout>;
  let searchGeneration = 0;
  const tauriAvailable = isTauriRuntime();

  // Cancel any pending debounce on unmount so a stale keystroke can't write the global search stores
  // after the component is gone (e.g. sidebar closed within the 250ms debounce).
  onDestroy(() => clearTimeout(debounceTimer));

  async function fetchSearchResults(trimmed: string, gen: number) {
    if (!trimmed) {
      searchResults.set(null);
      searchLoading.set(false);
      return;
    }

    searchLoading.set(true);
    if (!tauriAvailable) {
      searchResults.set(null);
      searchLoading.set(false);
      return;
    }

    try {
      const results = await api.searchSegments(trimmed);
      if (gen !== searchGeneration) return;
      searchResults.set(results);
    } catch (e) {
      if (gen !== searchGeneration) return;
      searchResults.set(null);
      notifications.error($t('searchFailed'), { detail: String(e) });
    } finally {
      if (gen === searchGeneration) searchLoading.set(false);
    }
  }

  function handleInput() {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
      const trimmed = query.trim();
      searchQuery.set(trimmed);
      const gen = ++searchGeneration;
      if (!trimmed) {
        searchResults.set(null);
        searchLoading.set(false);
        return;
      }
      fetchSearchResults(trimmed, gen);
    }, 250);
  }

  function handleClear() {
    clearTimeout(debounceTimer);
    searchGeneration++;
    query = '';
    searchQuery.set('');
    searchResults.set(null);
    searchLoading.set(false);
  }

  function setVerified(value: boolean | null) {
    filterVerified.set(value);
  }
</script>

<div class="space-y-2" data-testid="search-bar">
  <div class="relative">
    <svg
      class="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-cortex-500"
      fill="none"
      stroke="currentColor"
      viewBox="0 0 24 24"
    >
      <path
        stroke-linecap="round"
        stroke-linejoin="round"
        stroke-width="2"
        d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z"
      />
    </svg>
    <input
      type="search"
      data-testid="search-input"
      class="input ps-9 pe-8"
      placeholder={$t('searchPlaceholder')}
      bind:value={query}
      oninput={handleInput}
      aria-busy={$searchLoading}
    />
    {#if query}
      {#if $searchLoading}
        <svg
          class="absolute right-8 top-1/2 -translate-y-1/2 w-4 h-4 text-cortex-400 animate-spin"
          fill="none"
          viewBox="0 0 24 24"
          aria-hidden="true"
        >
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
      {/if}
      <button
        type="button"
        class="absolute right-3 top-1/2 -translate-y-1/2 text-cortex-500 hover:text-cortex-300"
        onclick={handleClear}
        aria-label={$t('clearSearch')}>✕</button
      >
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
        value={$sortOrder}
        oninput={(e) =>
          sortOrder.set(
            (e.target as HTMLSelectElement).value as
              | 'newest'
              | 'oldest'
              | 'duration'
              | 'verified'
              | 'confidence'
              | 'activeLearning',
          )}
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
