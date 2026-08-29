import { describe, expect, it, vi } from 'vitest';
import { drawWaveformCanvas } from '../../src/lib/waveformRenderer';

function canvasContext(measuredWidth: (text: string) => number = () => 4) {
  return {
    clearRect: vi.fn(),
    fillRect: vi.fn(),
    fillText: vi.fn(),
    measureText: vi.fn((text: string) => ({ width: measuredWidth(text) })),
    beginPath: vi.fn(),
    moveTo: vi.fn(),
    lineTo: vi.fn(),
    stroke: vi.fn(),
    arc: vi.fn(),
    fill: vi.fn(),
    setLineDash: vi.fn(),
    fillStyle: '',
    strokeStyle: '',
    lineWidth: 0,
    font: '',
    textAlign: '' as CanvasTextAlign,
  };
}

function renderOptions(
  overrides: Partial<Parameters<typeof drawWaveformCanvas>[1]> = {},
): Parameters<typeof drawWaveformCanvas>[1] {
  return {
    width: 60,
    height: 100,
    duration: 10,
    currentTime: 5,
    waveform: [-2, 0.5, 0],
    regions: [],
    words: [],
    color: '#6366f1',
    zoom: 1,
    selectionStart: null,
    selectionEnd: null,
    hoverTime: null,
    ...overrides,
  };
}

describe('drawWaveformCanvas', () => {
  it('clears but does not draw when duration or waveform authority is absent', () => {
    const zeroDuration = canvasContext();
    drawWaveformCanvas(
      zeroDuration as unknown as CanvasRenderingContext2D,
      renderOptions({ duration: 0 }),
    );
    expect(zeroDuration.clearRect).toHaveBeenCalledWith(0, 0, 60, 100);
    expect(zeroDuration.fillRect).not.toHaveBeenCalled();

    const noPeaks = canvasContext();
    drawWaveformCanvas(
      noPeaks as unknown as CanvasRenderingContext2D,
      renderOptions({ waveform: [] }),
    );
    expect(noPeaks.clearRect).toHaveBeenCalledOnce();
    expect(noPeaks.beginPath).not.toHaveBeenCalled();
  });

  it('draws regions, reversed selections, clamped peaks, word labels, playhead, and hover guide', () => {
    const context = canvasContext((text) => (text === 'too-wide' ? 999 : 4));
    drawWaveformCanvas(
      context as unknown as CanvasRenderingContext2D,
      renderOptions({
        regions: [
          { start: 1, end: 3 },
          { start: 4, end: 5, color: '#ff0000' },
        ],
        words: [
          { word: 'fits', start: 0, end: 5 },
          { word: 'too-wide', start: 5, end: 5.5 },
        ],
        zoom: 7,
        selectionStart: 8,
        selectionEnd: 2,
        hoverTime: 4,
      }),
    );

    expect(context.fillRect).toHaveBeenCalledWith(6, 18, 12, 64);
    expect(context.fillRect).toHaveBeenCalledWith(24, 18, 6, 64);
    expect(context.fillRect).toHaveBeenCalledWith(12, 18, 36, 64);
    expect(context.fillText).toHaveBeenCalledWith('fits', 15, 94);
    expect(context.fillText).not.toHaveBeenCalledWith('too-wide', expect.anything(), 94);
    expect(context.moveTo).toHaveBeenCalledWith(30, 0);
    expect(context.lineTo).toHaveBeenCalledWith(30, 100);
    expect(context.arc).toHaveBeenCalledWith(30, 3, 3, 0, Math.PI * 2);
    expect(context.setLineDash.mock.calls).toEqual([[[3, 3]], [[]]]);

    const barCalls = context.fillRect.mock.calls.filter(
      ([, y, width]) => width === 2 && typeof y === 'number' && y >= 18,
    );
    expect(barCalls.length).toBeGreaterThan(0);
    expect(barCalls.some(([, y]) => y === 18 + 32 - 28)).toBe(true);
  });

  it('selects the locked ruler step for low, medium, and high zoom', () => {
    const low = canvasContext();
    drawWaveformCanvas(
      low as unknown as CanvasRenderingContext2D,
      renderOptions({ duration: 12, zoom: 1 }),
    );
    expect(low.fillText.mock.calls.filter(([text]) => String(text).endsWith('s'))).toEqual([
      ['0.0s', 0, 13],
      ['10.0s', 50, 13],
    ]);

    const medium = canvasContext();
    drawWaveformCanvas(
      medium as unknown as CanvasRenderingContext2D,
      renderOptions({ duration: 3, zoom: 4 }),
    );
    expect(medium.fillText.mock.calls.filter(([text]) => String(text).endsWith('s'))).toEqual([
      ['0.0s', 0, 13],
      ['1.0s', 20, 13],
      ['2.0s', 40, 13],
      ['3.0s', 60, 13],
    ]);

    const high = canvasContext();
    drawWaveformCanvas(
      high as unknown as CanvasRenderingContext2D,
      renderOptions({ duration: 1, zoom: 7 }),
    );
    expect(high.fillText.mock.calls.filter(([text]) => String(text).endsWith('s'))).toEqual([
      ['0.0s', 0, 13],
      ['0.5s', 30, 13],
      ['1.0s', 60, 13],
    ]);
  });

  it('caps ruler work at 500 iterations for unusually long clips', () => {
    const context = canvasContext();
    drawWaveformCanvas(
      context as unknown as CanvasRenderingContext2D,
      renderOptions({ width: 3, duration: 10_000, zoom: 7 }),
    );

    const rulerLabels = context.fillText.mock.calls.filter(([text]) => String(text).endsWith('s'));
    expect(rulerLabels).toHaveLength(500);
    expect(rulerLabels.at(-1)?.[0]).toBe('249.5s');
  });
});
