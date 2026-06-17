<script lang="ts">
  import { t } from './i18n';

  let {
    view = 'curate',
    onSelect,
  }: {
    view?: string;
    onSelect: (id: string) => void;
  } = $props();

  // Primary workspaces. Settings sits at the bottom as a utility destination.
  const items = [
    { id: 'curate', labelKey: 'nav.curate', d: 'M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01' },
    { id: 'insights', labelKey: 'nav.insights', d: 'M4 19V5m5 14V10m5 9V8m5 11v-6' },
    { id: 'review', labelKey: 'nav.review', d: 'M3 8l9 5 9-5M3 8l9-5 9 5M3 8v9a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V8' },
  ];
</script>

<nav
  class="flex w-14 shrink-0 flex-col items-center gap-1.5 border-e border-line bg-surface-1 py-3"
  aria-label="Workspaces"
>
  {#each items as it (it.id)}
    <button
      type="button"
      class="relative flex h-10 w-10 items-center justify-center rounded-token transition-colors duration-150
        {view === it.id ? 'bg-accent-soft text-accent' : 'text-subtle hover:bg-surface-3 hover:text-default'}"
      aria-current={view === it.id ? 'page' : undefined}
      title={$t(it.labelKey)}
      aria-label={$t(it.labelKey)}
      onclick={() => onSelect(it.id)}
    >
      {#if view === it.id}
        <span class="absolute inset-y-2 start-0 w-0.5 rounded-full bg-accent"></span>
      {/if}
      <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
        <path d={it.d} />
      </svg>
    </button>
  {/each}

  <div class="mt-auto"></div>

  <button
    type="button"
    class="flex h-10 w-10 items-center justify-center rounded-token text-subtle transition-colors duration-150 hover:bg-surface-3 hover:text-default"
    title={$t('settings')}
    aria-label={$t('settings')}
    onclick={() => onSelect('settings')}
  >
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  </button>
</nav>
