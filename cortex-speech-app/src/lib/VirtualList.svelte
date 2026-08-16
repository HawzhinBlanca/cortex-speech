<script lang="ts">
  import { onMount } from 'svelte';
  import type { Snippet } from 'svelte';
  import type { SpeechSegment } from './types';
  import { dedupeById } from './dedupeById';

  interface Props {
    items: SpeechSegment[];
    itemHeight?: number;
    overscan?: number;
    children?: Snippet<[SpeechSegment]>;
    onSelect?: (item: SpeechSegment) => void;
    selectedId?: string | null;
    hasMore?: boolean;
    onEndReached?: () => void;
  }
  let {
    items,
    itemHeight = 64,
    overscan = 5,
    children,
    hasMore = false,
    onEndReached,
  }: Props = $props();

  let container: HTMLDivElement;
  let scrollTop = $state(0);
  let containerHeight = $state(600);

  // Render a by-id-unique view of the incoming items. The keyed {#each} below crashes the whole list
  // (Svelte each_key_duplicate) if two rows share an id, which can happen for one frame when an import
  // optimistically appends a segment the background reload also re-fetches. Deduping here makes the
  // list crash-proof regardless of what the parent passes; on unique input it is a same-order no-op.
  let rows = $derived(dedupeById(items));
  let totalHeight = $derived(rows.length * itemHeight);

  // Clamp scrollTop against the CURRENT content height. When `items` shrinks (e.g. a search filter
  // narrows the list) while scrolled down, the stale scrollTop is only reset by the browser's async
  // scroll-clamp event; until then a raw scrollTop would make startIndex exceed endIndex, yielding an
  // empty slice and a blank frame. Clamping keeps startIndex <= endIndex so matches render immediately.
  let effScroll = $derived(Math.min(scrollTop, Math.max(0, totalHeight - containerHeight)));

  let startIndex = $derived(Math.max(0, Math.floor(effScroll / itemHeight) - overscan));
  let endIndex = $derived(
    Math.min(rows.length, Math.ceil((effScroll + containerHeight) / itemHeight) + overscan),
  );
  let visibleItems = $derived(rows.slice(startIndex, endIndex));
  let offsetY = $derived(startIndex * itemHeight);

  // Fill a tall viewport without requiring a synthetic scroll, then continue in bounded pages.
  $effect(() => {
    if (hasMore && totalHeight <= containerHeight + itemHeight * overscan) onEndReached?.();
  });

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
    if (hasMore && scrollTop + containerHeight >= totalHeight - itemHeight * overscan) {
      onEndReached?.();
    }
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
