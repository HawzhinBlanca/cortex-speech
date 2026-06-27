<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { t } from './i18n';

  interface Props {
    waveform: number[];
    currentTime?: number;
    duration?: number;
    regions?: Array<{ start: number; end: number; color?: string }>;
    playing?: boolean;
    onSeek?: (time: number) => void;
    onRegionSelect?: (start: number, end: number) => void;
    color?: string;
    height?: number;
    wordTimestamps?: Array<{ word: string; start: number; end: number }>;
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

  const barWidth = 2;
  const barGap = 1;
  const rulerHeight = 18;
  const labelHeight = 18;

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
    const w = containerWidth * zoom;
    const h = height;
    ctx.clearRect(0, 0, w, h);

    // Guard: prevent division by zero and NaN propagation
    if (duration <= 0 || waveform.length === 0) return;

    // Waveform Background
    ctx.fillStyle = 'rgba(15, 23, 42, 0.4)'; // Slate-900 / 40%
    ctx.fillRect(0, 0, w, h);

    // Regions (Diarization tracks)
    for (const region of regions) {
      const x1 = (region.start / duration) * w;
      const x2 = (region.end / duration) * w;
      ctx.fillStyle = region.color || 'rgba(99, 102, 241, 0.1)';
      ctx.fillRect(x1, rulerHeight, x2 - x1, h - rulerHeight - labelHeight);
    }

    // Selection range (Shift drag)
    if (selectionStart !== null && selectionEnd !== null) {
      const x1 = (selectionStart / duration) * w;
      const x2 = (selectionEnd / duration) * w;
      ctx.fillStyle = 'rgba(34, 211, 238, 0.15)'; // Cyan select highlight
      ctx.fillRect(Math.min(x1, x2), rulerHeight, Math.abs(x2 - x1), h - rulerHeight - labelHeight);
    }

    // Waveform bars
    const numBars = Math.floor(w / (barWidth + barGap));
    const samplesPerBar = Math.max(1, Math.floor(waveform.length / numBars));
    const midY = rulerHeight + (h - rulerHeight - labelHeight) / 2;
    const maxBarH = (h - rulerHeight - labelHeight) / 2 - 4;

    ctx.fillStyle = color;
    for (let i = 0; i < numBars; i++) {
      const startIdx = i * samplesPerBar;
      const endIdx = Math.min(startIdx + samplesPerBar, waveform.length);
      let peak = 0;
      for (let j = startIdx; j < endIdx; j++) {
        peak = Math.max(peak, Math.abs(waveform[j]));
      }
      peak = Math.min(peak, 1.0);
      const barH = peak * maxBarH;
      const x = i * (barWidth + barGap);
      ctx.fillRect(x, midY - barH, barWidth, barH * 2);
    }

    // Timeline Ruler ticks
    ctx.fillStyle = 'rgba(255,255,255,0.03)';
    ctx.fillRect(0, 0, w, rulerHeight);

    const timeStep = zoom > 6 ? 0.5 : zoom > 3 ? 1.0 : 5.0; // Seconds between ticks
    ctx.fillStyle = '#64748b'; // Slate-500
    ctx.font = '8px monospace';
    ctx.textAlign = 'center';

    let t = 0.0;
    let tickCount = 0;
    const maxTicks = 500;
    while (t <= duration && tickCount++ < maxTicks) {
      const tx = (t / duration) * w;
      ctx.fillRect(tx - 0.5, 0, 1, rulerHeight - 4);
      if (t % (timeStep * 2.0) === 0.0 || zoom > 3) {
        ctx.fillText(`${t.toFixed(1)}s`, tx, rulerHeight - 5);
      }
      t += timeStep;
    }

    // Word Grid division ticks & Labels
    const labelY = h - labelHeight;
    ctx.fillStyle = 'rgba(255,255,255,0.03)';
    ctx.fillRect(0, labelY, w, labelHeight);

    if (wordTimestamps && wordTimestamps.length > 0) {
      ctx.font = '9px monospace';
      ctx.textAlign = 'center';
      for (const word of wordTimestamps) {
        const wx_start = (word.start / duration) * w;
        const wx_end = (word.end / duration) * w;
        const mid_x = (wx_start + wx_end) / 2;

        // Draw vertical dividing grid line
        ctx.fillStyle = 'rgba(255, 255, 255, 0.08)';
        ctx.fillRect(wx_start - 0.5, rulerHeight, 1, h - rulerHeight - labelHeight);

        // Draw word label in bottom track if it fits
        if (wx_end - wx_start > 14) {
          ctx.fillStyle = '#94a3b8'; // Slate-400
          const textWidth = ctx.measureText(word.word).width;
          if (textWidth < wx_end - wx_start) {
            ctx.fillText(word.word, mid_x, h - 6);
          }
        }
      }
    }

    // Active Playhead indicator
    if (duration > 0) {
      const playheadX = (currentTime / duration) * w;
      ctx.strokeStyle = '#f59e0b'; // Amber-500 neon playhead
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(playheadX, 0);
      ctx.lineTo(playheadX, h);
      ctx.stroke();

      // Playhead bulb at top
      ctx.fillStyle = '#f59e0b';
      ctx.beginPath();
      ctx.arc(playheadX, 3, 3, 0, Math.PI * 2);
      ctx.fill();
    }

    // Hover Scrub guide
    if (hoverTime !== null && duration > 0) {
      const hx = (hoverTime / duration) * w;
      ctx.strokeStyle = 'rgba(255,255,255,0.15)';
      ctx.lineWidth = 1;
      ctx.setLineDash([3, 3]);
      ctx.beginPath();
      ctx.moveTo(hx, 0);
      ctx.lineTo(hx, h);
      ctx.stroke();
      ctx.setLineDash([]);
    }
  }

  function getTimeFromEvent(e: MouseEvent | Touch): number {
    const rect = canvas.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const w = containerWidth * zoom;
    return (x / w) * duration;
  }

  function handlePointerDown(e: PointerEvent) {
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
</script>

<div
  class="relative w-full bg-cortex-950/40 backdrop-blur-md rounded-xl border border-cortex-800/50 p-3 flex flex-col gap-2"
>
  <!-- Timeline Zoom Control bar -->
  <div class="flex items-center justify-between text-[10px] text-cortex-400 font-mono px-1">
    <div class="flex items-center gap-1.5">
      <svg class="w-3 h-3 text-cortex-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path
          stroke-linecap="round"
          stroke-linejoin="round"
          stroke-width="2"
          d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0zM10 7v6m4-3H6"
        />
      </svg>
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
        aria-label="Waveform zoom slider"
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
      aria-label="Audio waveform timeline"
      aria-valuemin={0}
      aria-valuemax={duration}
      aria-valuenow={currentTime}
      tabindex="0"
      onpointerdown={handlePointerDown}
      onpointermove={handlePointerMove}
      onpointerup={handlePointerUp}
      onpointerleave={handlePointerLeave}
    ></canvas>
  </div>
</div>
