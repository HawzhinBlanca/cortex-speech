<script lang="ts">
  import { fade, scale } from 'svelte/transition';
  import { focusTrap } from './actions/focusTrap';

  type Size = 'sm' | 'md' | 'lg' | 'xl' | 'full';

  let {
    open = false,
    title = '',
    description = '',
    size = 'md',
    onClose = () => {},
    children,
    footer,
  }: {
    open?: boolean;
    title?: string;
    description?: string;
    size?: Size;
    onClose?: () => void;
    children?: import('svelte').Snippet;
    footer?: import('svelte').Snippet;
  } = $props();

  const widths: Record<Size, string> = {
    sm: 'max-w-sm',
    md: 'max-w-lg',
    lg: 'max-w-2xl',
    xl: 'max-w-4xl',
    full: 'max-w-[92vw]',
  };

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      e.preventDefault();
      onClose();
    }
  }
</script>

{#if open}
  <!-- Backdrop -->
  <div
    class="fixed inset-0 z-[100] flex items-center justify-center p-4 sm:p-6 glass"
    role="presentation"
    onclick={(e) => {
      if (e.target === e.currentTarget) onClose();
    }}
    transition:fade={{ duration: 140 }}
  >
    <!-- Dialog -->
    <div
      class="card relative flex max-h-[88vh] w-full flex-col overflow-hidden shadow-lift {widths[size]}"
      role="dialog"
      aria-modal="true"
      aria-label={title || undefined}
      aria-describedby={description ? 'modal-desc' : undefined}
      tabindex="-1"
      use:focusTrap
      onkeydown={onKeydown}
      transition:scale={{ duration: 180, start: 0.97, opacity: 0 }}
    >
      {#if title}
        <header class="flex shrink-0 items-start justify-between gap-4 border-b border-line px-5 py-4">
          <div class="min-w-0">
            <h2 class="truncate text-sm font-semibold text-default">{title}</h2>
            {#if description}
              <p id="modal-desc" class="mt-0.5 text-xs text-muted">{description}</p>
            {/if}
          </div>
          <button class="icon-btn -me-1" aria-label="Close dialog" onclick={onClose}>
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </button>
        </header>
      {/if}

      <div class="min-h-0 flex-1 overflow-auto">
        {@render children?.()}
      </div>

      {#if footer}
        <footer class="flex shrink-0 items-center justify-end gap-2 border-t border-line px-5 py-3.5">
          {@render footer()}
        </footer>
      {/if}
    </div>
  </div>
{/if}
