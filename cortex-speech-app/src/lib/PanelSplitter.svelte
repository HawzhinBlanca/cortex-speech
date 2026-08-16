<script lang="ts">
  interface Props {
    direction?: 'horizontal' | 'vertical';
    onResize: (delta: number) => void;
    label?: string;
    value: number;
    min?: number;
    max?: number;
  }

  let {
    direction = 'horizontal',
    onResize,
    label = 'Resize panel',
    value,
    min = 200,
    max = 600,
  }: Props = $props();

  let isDragging = $state(false);

  function handlePointerDown(e: PointerEvent) {
    isDragging = true;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
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

  function handleKeydown(e: KeyboardEvent) {
    const step = e.shiftKey ? 48 : 16;
    let delta = 0;
    if (direction === 'horizontal') {
      if (e.key === 'ArrowLeft') delta = -step;
      if (e.key === 'ArrowRight') delta = step;
    } else {
      if (e.key === 'ArrowUp') delta = -step;
      if (e.key === 'ArrowDown') delta = step;
    }
    if (delta === 0) return;
    e.preventDefault();
    onResize(delta);
  }
</script>

<!-- A focusable ARIA separator is an interactive widget per WAI-ARIA; Svelte's static-role table
     does not model that focusable specialization, so these two diagnostics are false positives. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
  class="relative flex items-center justify-center select-none group transition-colors duration-150 shrink-0 z-20
    {direction === 'horizontal'
    ? 'w-2 cursor-col-resize hover:bg-indigo-500/25 border-l border-r border-cortex-800/10'
    : 'h-2 cursor-row-resize hover:bg-indigo-500/25 border-t border-b border-cortex-800/10'}"
  role="separator"
  aria-orientation={direction === 'horizontal' ? 'vertical' : 'horizontal'}
  aria-label={label}
  aria-valuemin={min}
  aria-valuemax={max}
  aria-valuenow={Math.round(value)}
  aria-valuetext={`${Math.round(value)} pixels`}
  tabindex="0"
  onpointerdown={handlePointerDown}
  onpointermove={handlePointerMove}
  onpointerup={handlePointerUp}
  onkeydown={handleKeydown}
  style="touch-action: none;"
>
  <div
    class="rounded bg-cortex-700 group-hover:bg-indigo-300 group-focus-visible:bg-indigo-300 pointer-events-none {direction ===
    'horizontal'
      ? 'w-px h-8'
      : 'h-px w-8'}"
  ></div>
</div>
