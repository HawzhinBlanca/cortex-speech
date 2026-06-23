<script lang="ts">
  import { onMount } from 'svelte';
  import type { Snippet } from 'svelte';
  import type { SpeechSegment } from './types';

  interface Props {
    items: SpeechSegment[];
    itemHeight?: number;
    overscan?: number;
    children?: Snippet<[SpeechSegment]>;
    onSelect?: (item: SpeechSegment) => void;
    selectedId?: string | null;
  }
  let { items, itemHeight = 64, overscan = 5, children }: Props = $props();

  let container: HTMLDivElement;
  let scrollTop = $state(0);
  let containerHeight = $state(600);
  let totalHeight = $derived(items.length * itemHeight);

  let startIndex = $derived(Math.max(0, Math.floor(scrollTop / itemHeight) - overscan));
  let endIndex = $derived(
    Math.min(items.length, Math.ceil((scrollTop + containerHeight) / itemHeight) + overscan),
  );
  let visibleItems = $derived(items.slice(startIndex, endIndex));
  let offsetY = $derived(startIndex * itemHeight);

  onMount(() => {
    if (!container) return;
    containerHeight = container.clientHeight;
    // Keep the virtualization window in sync with the real viewport: a one-shot mount read goes stale
    // when the window/panel is later enlarged, leaving the bottom of the list rendered blank until the
    // user scrolls. Observe the container so endIndex tracks the actual height.
    const ro = new ResizeObserver(() => {
      containerHeight = container.clientHeight;
    });
    ro.observe(container);
    return () => ro.disconnect();
  });

  function handleScroll() {
    scrollTop = container.scrollTop;
  }
</script>

<div bind:this={container} class="overflow-y-auto h-full" onscroll={handleScroll} role="group">
  <div style="height: {totalHeight}px; position: relative;">
    <div style="position: absolute; top: 0; left: 0; right: 0; transform: translateY({offsetY}px);">
      {#each visibleItems as item (item.id)}
        <div style="height: {itemHeight}px">
          {#if children}
            {@render children(item)}
          {/if}
        </div>
      {/each}
    </div>
  </div>
</div>
