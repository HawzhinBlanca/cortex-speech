<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { t } from './i18n';
  import { drawWaveformCanvas, type WaveformRegion, type WaveformWord } from './waveformRenderer';

  const isolate = (value: string | number): string =>
    `${String.fromCodePoint(0x2068)}${String(value)}${String.fromCodePoint(0x2069)}`;

  interface Props {
    waveform: number[];
    currentTime?: number;
    duration?: number;
    regions?: WaveformRegion[];
    playing?: boolean;
    onSeek?: (time: number) => void;
    onRegionSelect?: (start: number, end: number) => void;
    color?: string;
    height?: number;
    wordTimestamps?: WaveformWord[];
    disabled?: boolean;
    disabledDescriptionId?: string;
  }

  let {
    waveform = [],
    currentTime = 0,
    duration = 0,
    regions = [],
    playing = false,
    onSeek,
    onRegionSelect,
    color = '#6366f1', // Indigo-500 premium color
    height = 100,
    wordTimestamps = [],
    disabled = false,
    disabledDescriptionId,
  }: Props = $props();

  let canvas: HTMLCanvasElement;
  let scrollContainer: HTMLDivElement;
  let ctx: CanvasRenderingContext2D | null = null;
  let containerWidth = $state(600);
  let zoom = $state(1.0); // Zoom factor (1.0x to 10.0x)
  let isDragging = $state(false);
  let hoverTime = $state<number | null>(null);
  let selectionStart: number | null = null;
  let selectionEnd: number | null = null;
  let animationId = 0;

  onMount(() => {
    ctx = canvas.getContext('2d');
    if (!ctx) {
      console.error('Waveform: failed to acquire canvas 2D context');
      return;
    }
    const ro = new ResizeObserver(() => {
      if (!scrollContainer) return;
      containerWidth = scrollContainer.clientWidth ?? 600;
      updateCanvasDimensions();
      draw();
    });
    ro.observe(scrollContainer);
    return () => ro.disconnect();
  });

  function updateCanvasDimensions() {
    if (!canvas) return;
    const w = containerWidth * zoom;
    canvas.width = w * devicePixelRatio;
    canvas.height = height * devicePixelRatio;
    canvas.style.width = `${w}px`;
    canvas.style.height = `${height}px`;
    if (ctx) {
      ctx.setTransform(1, 0, 0, 1, 0, 0); // reset scale
      ctx.scale(devicePixelRatio, devicePixelRatio);
    }
  }

  // Keep the playhead visible during playback — but only nudge when it actually LEAVES the viewport.
  // Forcing scrollLeft to re-centre every animation frame fought the user's manual horizontal scroll
  // (it snapped straight back), making the waveform un-scrollable while playing.
  $effect(() => {
    if (scrollContainer && duration > 0 && playing && !isDragging) {
      const w = containerWidth * zoom;
      const playheadX = (currentTime / duration) * w;
      const viewLeft = scrollContainer.scrollLeft;
      const viewRight = viewLeft + scrollContainer.clientWidth;
      if (playheadX < viewLeft || playheadX > viewRight) {
        scrollContainer.scrollLeft = playheadX - scrollContainer.clientWidth / 2;
      }
    }
  });

  function track(..._args: unknown[]) {}

  // Re-draw and re-scale on Zoom changes
  $effect(() => {
    track(zoom);
    updateCanvasDimensions();
    draw();
  });

  // Data-change effect: redraw when waveform data, regions, or word timestamps change
  $effect(() => {
    track(waveform, regions, wordTimestamps);
    draw();
  });

  // Playback animation loop: only start/stop when `playing` changes
  $effect(() => {
    if (playing) {
      cancelAnimationFrame(animationId);
      const animate = () => {
        animationId = requestAnimationFrame(animate);
        draw();
      };
      animationId = requestAnimationFrame(animate);
    } else {
      cancelAnimationFrame(animationId);
    }
  });

  $effect(() => {
    if (!disabled) return;
    isDragging = false;
    selectionStart = null;
    selectionEnd = null;
    hoverTime = null;
  });

  // While PAUSED no animation loop runs, so a seek that changes currentTime (the AudioPlayer scrubber,
  // a word-chip seek, a programmatic jump) would leave the playhead frozen at the old position. Redraw
  // on currentTime change when not playing; during playback the rAF loop above already repaints, so the
  // `!playing` guard skips the redundant draw. (currentTime was previously tracked by the data-change
  // effect; splitting out the animation loop dropped it — this restores the paused-seek repaint.)
  $effect(() => {
    track(currentTime);
    if (!playing) draw();
  });

  onDestroy(() => cancelAnimationFrame(animationId));

  function draw() {
    if (!ctx) return;
    drawWaveformCanvas(ctx, {
      width: containerWidth * zoom,
      height,
      duration,
      currentTime,
      waveform,
      regions,
      words: wordTimestamps,
      color,
      zoom,
      selectionStart,
      selectionEnd,
      hoverTime,
    });
  }

  function getTimeFromEvent(e: MouseEvent | Touch): number {
    const rect = canvas.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const w = containerWidth * zoom;
    return (x / w) * duration;
  }

  function handlePointerDown(e: PointerEvent) {
    if (disabled) return;
    isDragging = true;
    const t = getTimeFromEvent(e);
    if (e.shiftKey && onRegionSelect) {
      selectionStart = t;
      selectionEnd = null;
    } else if (onSeek) {
      onSeek(Math.max(0, Math.min(t, duration)));
    }
    canvas.setPointerCapture(e.pointerId);
  }

  function handlePointerMove(e: PointerEvent) {
    if (disabled) return;
    const t = getTimeFromEvent(e);
    hoverTime = t;
    if (isDragging && selectionStart !== null) {
      selectionEnd = t;
    }
    if (isDragging && onSeek && !e.shiftKey) {
      onSeek(Math.max(0, Math.min(t, duration)));
    }
    // hoverTime/selectionEnd are read imperatively inside draw(), not via a reactive $effect, and while
    // PAUSED no redraw loop is running — so repaint here or the hover scrub guide never follows the cursor.
    draw();
  }

  function handlePointerUp(_e: PointerEvent) {
    if (disabled) {
      isDragging = false;
      selectionStart = null;
      selectionEnd = null;
      return;
    }
    isDragging = false;
    if (selectionStart !== null && selectionEnd !== null && onRegionSelect) {
      const start = Math.min(selectionStart, selectionEnd);
      const end = Math.max(selectionStart, selectionEnd);
      if (end - start > 0.1) onRegionSelect(start, end);
      selectionStart = null;
      selectionEnd = null;
    }
  }

  function handlePointerLeave() {
    hoverTime = null;
    if (!isDragging) {
      selectionStart = null;
      selectionEnd = null;
    }
  }

  function handleTimelineKeydown(e: KeyboardEvent) {
    if (disabled || !onSeek || duration <= 0) return;
    const step = e.shiftKey ? 5 : 1;
    let next: number | null = null;
    switch (e.key) {
      case 'ArrowLeft':
      case 'ArrowDown':
        next = currentTime - step;
        break;
      case 'ArrowRight':
      case 'ArrowUp':
        next = currentTime + step;
        break;
      case 'Home':
        next = 0;
        break;
      case 'End':
        next = duration;
        break;
    }
    if (next === null) return;
    e.preventDefault();
    onSeek(Math.max(0, Math.min(next, duration)));
  }
</script>

<div
  class="relative w-full bg-cortex-950/40 backdrop-blur-md rounded-xl border border-cortex-800/50 p-3 flex flex-col gap-2"
>
  <!-- Timeline Zoom Control bar -->
  <div class="flex items-center justify-between text-[10px] text-cortex-400 font-mono px-1">
    <div class="flex items-center gap-1.5">
      <span>{$t('waveform.timelineZoom')}</span>
    </div>
    <div class="flex items-center gap-2">
      <input
        type="range"
        min="1.0"
        max="10.0"
        step="0.5"
        bind:value={zoom}
        class="w-32 h-1 bg-cortex-800 rounded-lg appearance-none cursor-pointer accent-indigo-500 focus:outline-none"
        aria-label={$t('waveform.zoomSlider')}
        dir="ltr"
      />
      <span class="w-8 text-end font-bold text-indigo-400">{zoom.toFixed(1)}x</span>
    </div>
  </div>

  <!-- Audio Waveform Canvas Box -->
  <div
    bind:this={scrollContainer}
    class="w-full overflow-x-auto relative rounded-lg border border-cortex-800/30 scroll-smooth shadow-inner"
    style="scrollbar-width: thin; scrollbar-color: #4f46e5 transparent;"
  >
    <canvas
      bind:this={canvas}
      class="block cursor-pointer select-none"
      role="slider"
      aria-label={$t('waveform.audioTimeline')}
      aria-valuemin={0}
      aria-valuemax={duration}
      aria-valuenow={currentTime}
      aria-valuetext={$t('waveform.position', {
        current: isolate(currentTime.toFixed(1)),
        duration: isolate(duration.toFixed(1)),
      })}
      aria-disabled={disabled}
      aria-describedby={disabled ? disabledDescriptionId : undefined}
      tabindex={disabled ? -1 : 0}
      onkeydown={handleTimelineKeydown}
      onpointerdown={handlePointerDown}
      onpointermove={handlePointerMove}
      onpointerup={handlePointerUp}
      onpointerleave={handlePointerLeave}
    ></canvas>
  </div>
</div>
