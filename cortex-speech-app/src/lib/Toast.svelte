<script lang="ts">
  import { notifications, type Notification } from './stores/notificationStore';
  import { fly, fade } from 'svelte/transition';
  import { flip } from 'svelte/animate';
  import { t } from './i18n';

  let items: Notification[] = $state([]);
  notifications.subscribe((n) => (items = n));

  const BAR: Record<string, string> = {
    success: 'bg-success',
    error: 'bg-danger',
    warning: 'bg-warning',
    info: 'bg-info',
  };
</script>

<div
  class="pointer-events-none fixed bottom-4 end-4 z-[120] flex w-[calc(100%-2rem)] max-w-sm flex-col gap-2.5"
  aria-live="polite"
>
  {#each items as notif (notif.id)}
    <div
      class="card pointer-events-auto relative flex items-start gap-3 overflow-hidden p-3.5 pe-10 shadow-lift"
      role="alert"
      in:fly={{ y: 14, duration: 240 }}
      out:fade={{ duration: 160 }}
      animate:flip={{ duration: 220 }}
    >
      <span class="absolute inset-y-0 start-0 w-1 {BAR[notif.type] ?? 'bg-accent'}"></span>

      <div class="min-w-0 flex-1">
        <p class="text-sm font-medium text-default">{notif.message}</p>
        {#if notif.detail}
          <bdi class="mt-0.5 block truncate text-xs text-muted" dir="auto">{notif.detail}</bdi>
        {/if}
        {#if notif.action}
          <button
            class="mt-1.5 text-xs font-medium text-accent hover:underline"
            onclick={() => {
              notif.action?.handler();
              notifications.dismiss(notif.id);
            }}>{notif.action.label}</button
          >
        {/if}
      </div>

      <button
        class="absolute end-2 top-2 rounded px-1.5 py-1 text-[10px] text-muted hover:bg-surface-3 hover:text-default"
        onclick={() => notifications.dismiss(notif.id)}
        aria-label={$t('dismiss')}
      >
        {$t('dismiss')}
      </button>
    </div>
  {/each}
</div>
