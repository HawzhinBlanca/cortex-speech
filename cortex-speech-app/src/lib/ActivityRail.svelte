<script lang="ts">
  import ChartNoAxesColumnIncreasing from '@lucide/svelte/icons/chart-no-axes-column-increasing';
  import Inbox from '@lucide/svelte/icons/inbox';
  import List from '@lucide/svelte/icons/list';
  import Settings from '@lucide/svelte/icons/settings';
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
    {
      id: 'curate',
      labelKey: 'nav.curate',
      icon: List,
    },
    { id: 'insights', labelKey: 'nav.insights', icon: ChartNoAxesColumnIncreasing },
    {
      id: 'review',
      labelKey: 'nav.review',
      icon: Inbox,
    },
  ] as const;
</script>

<nav
  data-testid="activity-rail"
  class="activity-rail flex w-14 shrink-0 flex-col items-center gap-1.5 border-e border-line bg-surface-1 py-3"
  aria-label={$t('workspaces')}
>
  {#each items as it (it.id)}
    {@const Icon = it.icon}
    <button
      type="button"
      class="ring-focus relative flex h-10 w-10 items-center justify-center rounded-token transition-colors duration-150
        {view === it.id
        ? 'bg-accent-soft text-accent'
        : 'text-subtle hover:bg-surface-3 hover:text-default'}"
      aria-current={view === it.id ? 'page' : undefined}
      title={$t(it.labelKey)}
      aria-label={$t(it.labelKey)}
      onclick={() => onSelect(it.id)}
    >
      {#if view === it.id}
        <span class="absolute inset-y-2 start-0 w-0.5 rounded-full bg-accent"></span>
      {/if}
      <Icon size={20} strokeWidth={1.6} aria-hidden="true" />
    </button>
  {/each}

  <div class="mt-auto"></div>

  <button
    type="button"
    class="ring-focus flex h-10 w-10 items-center justify-center rounded-token text-subtle transition-colors duration-150 hover:bg-surface-3 hover:text-default"
    title={$t('settings')}
    aria-label={$t('settings')}
    onclick={() => onSelect('settings')}
  >
    <Settings size={20} strokeWidth={1.6} aria-hidden="true" />
  </button>
</nav>

<style>
  /* At the 320–499px full-reflow tier, 56px of permanent rail would remove a quarter of the review
     canvas and force its transport onto a horizontal scrollbar. The same destinations remain in the
     header overflow and command palette, so hiding the duplicate rail preserves both reachability and
     the usable correction surface. */
  @media (max-width: 499px) {
    .activity-rail {
      display: none;
    }
  }
</style>
