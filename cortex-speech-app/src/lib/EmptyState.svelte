<script lang="ts">
  import Archive from '@lucide/svelte/icons/archive';
  import CircleAlert from '@lucide/svelte/icons/circle-alert';
  import Mic from '@lucide/svelte/icons/mic';
  import Search from '@lucide/svelte/icons/search';
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

  const StateIcon = $derived(
    variant === 'search'
      ? Search
      : variant === 'error'
        ? CircleAlert
        : variant === 'mic'
          ? Mic
          : Archive,
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
    <StateIcon size={26} strokeWidth={1.5} aria-hidden="true" />
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
