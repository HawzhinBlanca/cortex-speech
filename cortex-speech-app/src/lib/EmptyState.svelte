<script lang="ts">
  // A consistent, designed empty / no-results / error state. Pass CTA buttons as
  // children. Fades in; fully token-driven so it works in light and dark.
  let {
    variant = 'empty',
    title,
    description = '',
    compact = false,
    children,
  }: {
    variant?: 'empty' | 'search' | 'error' | 'mic';
    title: string;
    description?: string;
    compact?: boolean;
    children?: import('svelte').Snippet;
  } = $props();

  const chip = $derived(
    variant === 'error'
      ? 'bg-danger/10 text-danger'
      : variant === 'search'
        ? 'bg-surface-2 text-subtle'
        : 'bg-accent-soft text-accent',
  );
</script>

<!-- min-h-full + `safe center`, NOT h-full + plain centering. Measured at a 640x400 viewport (a
     1280x800 screen at 200 % zoom): `h-full` pinned this box to the scroller's height while its own
     content was taller, and plain `justify-center` then split the overflow evenly — pushing the icon
     and heading ABOVE scrollTop 0, where nothing can scroll to them, while the call-to-action stayed
     below the fold and could not be reached either. `safe center` degrades to flex-start exactly when
     centering would overflow, and min-h-full lets the box grow so the scroller can reach all of it. -->
<div
  class="flex min-h-full flex-col items-center [justify-content:safe_center] gap-3 px-6 text-center animate-fade-in"
  class:py-10={compact}
>
  <div class="flex h-14 w-14 items-center justify-center rounded-2xl {chip}">
    <svg
      width="26"
      height="26"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
    >
      {#if variant === 'search'}
        <circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" />
      {:else if variant === 'error'}
        <circle cx="12" cy="12" r="9" /><path d="M12 8v4M12 16h.01" />
      {:else if variant === 'mic'}
        <path d="M12 18a3 3 0 0 0 3-3V5a3 3 0 0 0-6 0v10a3 3 0 0 0 3 3Z" />
        <path d="M19 11a7 7 0 0 1-14 0M12 18v4M8 22h8" />
      {:else}
        <path d="M5 7 7 4h10l2 3M5 7v11a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V7M5 7h14M9 12h6" />
      {/if}
    </svg>
  </div>

  <div class="max-w-[17rem]">
    <p class="text-sm font-semibold text-default">{title}</p>
    {#if description}
      <p class="mt-1 text-xs leading-relaxed text-muted">{description}</p>
    {/if}
  </div>

  {#if children}
    <div class="mt-1 flex flex-wrap items-center justify-center gap-2">{@render children()}</div>
  {/if}
</div>
