export interface WaveformRegion {
  start: number;
  end: number;
  color?: string;
}

export interface WaveformWord {
  word: string;
  start: number;
  end: number;
}

interface WaveformRenderOptions {
  width: number;
  height: number;
  duration: number;
  currentTime: number;
  waveform: readonly number[];
  regions: readonly WaveformRegion[];
  words: readonly WaveformWord[];
  color: string;
  zoom: number;
  selectionStart: number | null;
  selectionEnd: number | null;
  hoverTime: number | null;
}

const BAR_WIDTH = 2;
const BAR_GAP = 1;
const RULER_HEIGHT = 18;
const LABEL_HEIGHT = 18;

export function drawWaveformCanvas(
  context: CanvasRenderingContext2D,
  options: WaveformRenderOptions,
): void {
  const {
    width: w,
    height: h,
    duration,
    currentTime,
    waveform,
    regions,
    words,
    color,
    zoom,
  } = options;
  context.clearRect(0, 0, w, h);
  if (duration <= 0 || waveform.length === 0) return;

  context.fillStyle = 'rgba(15, 23, 42, 0.4)';
  context.fillRect(0, 0, w, h);
  for (const region of regions) {
    const x1 = (region.start / duration) * w;
    const x2 = (region.end / duration) * w;
    context.fillStyle = region.color || 'rgba(99, 102, 241, 0.1)';
    context.fillRect(x1, RULER_HEIGHT, x2 - x1, h - RULER_HEIGHT - LABEL_HEIGHT);
  }

  if (options.selectionStart !== null && options.selectionEnd !== null) {
    const x1 = (options.selectionStart / duration) * w;
    const x2 = (options.selectionEnd / duration) * w;
    context.fillStyle = 'rgba(34, 211, 238, 0.15)';
    context.fillRect(
      Math.min(x1, x2),
      RULER_HEIGHT,
      Math.abs(x2 - x1),
      h - RULER_HEIGHT - LABEL_HEIGHT,
    );
  }

  const numBars = Math.floor(w / (BAR_WIDTH + BAR_GAP));
  const midY = RULER_HEIGHT + (h - RULER_HEIGHT - LABEL_HEIGHT) / 2;
  const maxBarHeight = (h - RULER_HEIGHT - LABEL_HEIGHT) / 2 - 4;
  context.fillStyle = color;
  for (let index = 0; index < numBars; index += 1) {
    // Fractional slices stretch a fixed peak array across the full canvas at every zoom level.
    const startIndex = Math.floor((index * waveform.length) / numBars);
    const endIndex = Math.max(
      startIndex + 1,
      Math.floor(((index + 1) * waveform.length) / numBars),
    );
    let peak = 0;
    for (let sample = startIndex; sample < Math.min(endIndex, waveform.length); sample += 1) {
      peak = Math.max(peak, Math.abs(waveform[sample]));
    }
    const barHeight = Math.min(peak, 1) * maxBarHeight;
    const x = index * (BAR_WIDTH + BAR_GAP);
    context.fillRect(x, midY - barHeight, BAR_WIDTH, barHeight * 2);
  }

  context.fillStyle = 'rgba(255,255,255,0.03)';
  context.fillRect(0, 0, w, RULER_HEIGHT);
  const timeStep = zoom > 6 ? 0.5 : zoom > 3 ? 1 : 5;
  context.fillStyle = '#64748b';
  context.font = '8px monospace';
  context.textAlign = 'center';
  let tick = 0;
  let tickCount = 0;
  while (tick <= duration && tickCount++ < 500) {
    const x = (tick / duration) * w;
    context.fillRect(x - 0.5, 0, 1, RULER_HEIGHT - 4);
    if (tick % (timeStep * 2) === 0 || zoom > 3) {
      context.fillText(`${tick.toFixed(1)}s`, x, RULER_HEIGHT - 5);
    }
    tick += timeStep;
  }

  const labelY = h - LABEL_HEIGHT;
  context.fillStyle = 'rgba(255,255,255,0.03)';
  context.fillRect(0, labelY, w, LABEL_HEIGHT);
  if (words.length > 0) {
    context.font = '9px monospace';
    context.textAlign = 'center';
    for (const word of words) {
      const startX = (word.start / duration) * w;
      const endX = (word.end / duration) * w;
      context.fillStyle = 'rgba(255, 255, 255, 0.08)';
      context.fillRect(startX - 0.5, RULER_HEIGHT, 1, h - RULER_HEIGHT - LABEL_HEIGHT);
      if (endX - startX > 14 && context.measureText(word.word).width < endX - startX) {
        context.fillStyle = '#94a3b8';
        context.fillText(word.word, (startX + endX) / 2, h - 6);
      }
    }
  }

  const playheadX = (currentTime / duration) * w;
  context.strokeStyle = '#f59e0b';
  context.lineWidth = 2;
  context.beginPath();
  context.moveTo(playheadX, 0);
  context.lineTo(playheadX, h);
  context.stroke();
  context.fillStyle = '#f59e0b';
  context.beginPath();
  context.arc(playheadX, 3, 3, 0, Math.PI * 2);
  context.fill();

  if (options.hoverTime !== null) {
    const hoverX = (options.hoverTime / duration) * w;
    context.strokeStyle = 'rgba(255,255,255,0.15)';
    context.lineWidth = 1;
    context.setLineDash([3, 3]);
    context.beginPath();
    context.moveTo(hoverX, 0);
    context.lineTo(hoverX, h);
    context.stroke();
    context.setLineDash([]);
  }
}
