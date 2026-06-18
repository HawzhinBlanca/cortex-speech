<script lang="ts">
  interface Props {
    direction?: 'horizontal' | 'vertical';
    onResize: (delta: number) => void;
  }

  let { direction = 'horizontal', onResize }: Props = $props();

  let isDragging = $state(false);

  function handlePointerDown(e: PointerEvent) {
    isDragging = true;
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    e.preventDefault();
  }

  function handlePointerMove(e: PointerEvent) {
    if (!isDragging) return;
    if (direction === 'horizontal') {
      onResize(e.movementX);
    } else {
      onResize(e.movementY);
    }
  }

  function handlePointerUp(_e: PointerEvent) {
    isDragging = false;
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="relative flex items-center justify-center cursor-col-resize select-none group transition-all duration-150 shrink-0 z-20
    {direction === 'horizontal'
    ? 'w-[3px] hover:w-1.5 bg-cortex-900/35 hover:bg-indigo-500/60 border-l border-r border-cortex-800/10'
    : 'h-[3px] hover:h-1.5 bg-cortex-900/35 hover:bg-indigo-500/60 border-t border-b border-cortex-800/10'}"
  onpointerdown={handlePointerDown}
  onpointermove={handlePointerMove}
  onpointerup={handlePointerUp}
  style="touch-action: none;"
>
  <!-- Mini centering grip grip line -->
  <div class="hidden group-hover:block w-1 h-8 rounded bg-indigo-300 pointer-events-none"></div>
</div>
