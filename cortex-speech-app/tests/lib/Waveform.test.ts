import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import Waveform from '../../src/lib/Waveform.svelte';

describe('Waveform keyboard seeking', () => {
  beforeEach(() => {
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
    vi.spyOn(console, 'error').mockImplementation(() => {});
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it('supports arrows, accelerated shift-arrows, Home, and End', async () => {
    const onSeek = vi.fn();
    render(Waveform, { waveform: [0.2, 0.4], currentTime: 5, duration: 10, onSeek });
    const timeline = screen.getByRole('slider', { name: 'Audio waveform timeline' });

    await fireEvent.keyDown(timeline, { key: 'ArrowRight' });
    await fireEvent.keyDown(timeline, { key: 'ArrowLeft', shiftKey: true });
    await fireEvent.keyDown(timeline, { key: 'Home' });
    await fireEvent.keyDown(timeline, { key: 'End' });

    expect(onSeek.mock.calls.map(([value]) => value)).toEqual([6, 0, 0, 10]);
    expect(timeline).toHaveAttribute('aria-valuetext', '5.0 seconds of 10.0 seconds');
  });
});
