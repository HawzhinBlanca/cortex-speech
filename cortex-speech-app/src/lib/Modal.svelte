<script lang="ts">
  import { fade, scale } from 'svelte/transition';
  import { focusTrap } from './actions/focusTrap';
  import { t } from './i18n';

  type Size = 'sm' | 'md' | 'lg' | 'xl' | 'full';

  let {
    open = false,
    title = '',
    ariaLabel = '',
    description = '',
    size = 'md',
    testid = '',
    onClose = () => {},
    children,
    footer,
  }: {
    open?: boolean;
    title?: string;
    ariaLabel?: string;
    description?: string;
    size?: Size;
    testid?: string;
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
    class="modal-backdrop fixed inset-0 z-[100] flex items-center justify-center overflow-y-auto p-4 sm:p-6 glass"
    role="presentation"
    onclick={(e) => {
      if (e.target === e.currentTarget) onClose();
    }}
    transition:fade={{ duration: 140 }}
  >
    <!-- Dialog -->
    <div
      class="modal-dialog card relative flex max-h-[88vh] w-full flex-col overflow-hidden shadow-lift {widths[
        size
      ]}"
      role="dialog"
      aria-modal="true"
      data-testid={testid || undefined}
      aria-label={ariaLabel || title || undefined}
      aria-describedby={description ? 'modal-desc' : undefined}
      tabindex="-1"
      use:focusTrap
      onkeydown={onKeydown}
      transition:scale={{ duration: 180, start: 0.97, opacity: 0 }}
    >
      {#if title}
        <header
          class="flex shrink-0 items-start justify-between gap-4 border-b border-line px-5 py-4"
        >
          <div class="min-w-0">
            <h2 class="truncate text-sm font-semibold text-default">{title}</h2>
            {#if description}
              <p id="modal-desc" class="mt-0.5 text-xs text-muted">{description}</p>
            {/if}
          </div>
          <button class="btn-ghost -me-1 text-xs" aria-label={$t('close')} onclick={onClose}>
            {$t('close')}
          </button>
        </header>
      {/if}

      <div class="modal-body min-h-0 flex-1 overflow-auto">
        {@render children?.()}
      </div>

      {#if footer}
        <footer
          class="flex min-w-0 shrink-0 flex-wrap items-center justify-end gap-2 border-t border-line px-5 py-3.5"
        >
          {@render footer()}
        </footer>
      {/if}
    </div>
  </div>
{/if}

<style>
  /* At 400% zoom a 720px-tall window can expose only 180 CSS pixels. In that geometry a fixed
     header and footer used to squeeze the message to zero height. Let the complete dialog become
     one scrollable document instead, so its explanation and every action remain reachable. */
  @media (max-height: 360px) {
    .modal-backdrop {
      align-items: flex-start;
      padding-block: 0.25rem;
    }

    .modal-dialog {
      flex: none;
      max-height: none;
      overflow: visible;
      margin-block: 0;
    }

    .modal-body {
      flex: none;
      overflow: visible;
    }
  }
</style>
