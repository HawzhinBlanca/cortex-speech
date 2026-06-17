<script lang="ts">
  import { notifications, type Notification } from './stores/notificationStore';

  let items: Notification[] = $state([]);

  notifications.subscribe(n => items = n);

  function icon(type: string) {
    switch (type) {
      case 'success': return '✓';
      case 'error': return '✕';
      case 'warning': return '⚠';
      case 'info': return 'ℹ';
      default: return '';
    }
  }

  function color(type: string): string {
    switch (type) {
      case 'success': return 'border-emerald-500/50 bg-emerald-900/40 text-emerald-200';
      case 'error': return 'border-red-500/50 bg-red-900/40 text-red-200';
      case 'warning': return 'border-amber-500/50 bg-amber-900/40 text-amber-200';
      case 'info': return 'border-cortex-500/50 bg-cortex-900/40 text-cortex-200';
      default: return 'border-cortex-700 bg-cortex-900 text-gray-200';
    }
  }
</script>

<div class="fixed bottom-4 right-4 z-50 flex flex-col gap-2 max-w-sm w-full pointer-events-none" aria-live="polite">
  {#if items.length > 0}
    {#each items as notif (notif.id)}
      <div
        class="pointer-events-auto flex items-start gap-3 px-4 py-3 rounded-lg border backdrop-blur-md shadow-2xl transition-all duration-300 {color(notif.type)}"
        role="alert"
      >
        <span class="text-lg mt-0.5 shrink-0">{icon(notif.type)}</span>
        <div class="flex-1 min-w-0">
          <p class="text-sm font-medium">{notif.message}</p>
          {#if notif.detail}
            <p class="text-xs mt-0.5 opacity-70 truncate">{notif.detail}</p>
          {/if}
          {#if notif.action}
            <button
              class="text-xs mt-1 underline opacity-80 hover:opacity-100"
              onclick={() => { notif.action?.handler(); notifications.dismiss(notif.id); }}
            >{notif.action.label}</button>
          {/if}
          {#if notif.progress !== undefined}
            <div class="mt-2 h-1 bg-black/20 rounded-full overflow-hidden">
              <div
                class="h-full bg-current rounded-full transition-all duration-300"
                style="width: {notif.progress}%"
              ></div>
            </div>
          {/if}
        </div>
        <button
          class="text-xs opacity-60 hover:opacity-100 transition-opacity shrink-0"
          onclick={() => notifications.dismiss(notif.id)}
          aria-label="Dismiss"
        >✕</button>
      </div>
    {/each}
  {/if}
</div>
