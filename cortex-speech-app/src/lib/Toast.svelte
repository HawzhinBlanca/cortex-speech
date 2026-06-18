<script lang="ts">
  import { notifications, type Notification } from './stores/notificationStore';
  import { fly, fade } from 'svelte/transition';
  import { flip } from 'svelte/animate';

  let items: Notification[] = $state([]);
  notifications.subscribe((n) => (items = n));

  const ICONS: Record<string, string> = { success: '✓', error: '✕', warning: '⚠', info: 'ℹ' };
  const TINT: Record<string, string> = {
    success: 'text-success',
    error: 'text-danger',
    warning: 'text-warning',
    info: 'text-info',
  };
  const BAR: Record<string, string> = {
    success: 'bg-success',
    error: 'bg-danger',
    warning: 'bg-warning',
    info: 'bg-info',
  };
</script>

<div
  class="pointer-events-none fixed bottom-4 end-4 z-[120] flex w-full max-w-sm flex-col gap-2.5"
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

      <span
        class="mt-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-surface-3 text-sm {TINT[
          notif.type
        ] ?? 'text-accent'}"
      >
        {ICONS[notif.type] ?? '•'}
      </span>

      <div class="min-w-0 flex-1">
        <p class="text-sm font-medium text-default">{notif.message}</p>
        {#if notif.detail}
          <p class="mt-0.5 truncate text-xs text-muted">{notif.detail}</p>
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
        {#if notif.progress !== undefined}
          <div class="mt-2 h-1 overflow-hidden rounded-full bg-surface-3">
            <div
              class="h-full rounded-full {BAR[notif.type] ??
                'bg-accent'} transition-[width] duration-300 ease-smooth"
              style="width: {notif.progress}%"
            ></div>
          </div>
        {/if}
      </div>

      <button
        class="icon-btn absolute end-1.5 top-1.5 h-7 w-7"
        onclick={() => notifications.dismiss(notif.id)}
        aria-label="Dismiss"
      >
        <svg
          width="14"
          height="14"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
        >
          <path d="M18 6 6 18M6 6l12 12" />
        </svg>
      </button>
    </div>
  {/each}
</div>
