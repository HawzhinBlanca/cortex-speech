import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import Waveform from '../../src/lib/Waveform.svelte';
import { locale } from '../../src/lib/i18n';
import { en } from '../../src/lib/i18n/en';

const isolate = (value: string): string =>
  `${String.fromCodePoint(0x2068)}${value}${String.fromCodePoint(0x2069)}`;

describe('Waveform keyboard seeking', () => {
  beforeEach(() => {
    locale.set('en');
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
    const timeline = screen.getByRole('slider', { name: en['waveform.audioTimeline'] });

    await fireEvent.keyDown(timeline, { key: 'ArrowRight' });
    await fireEvent.keyDown(timeline, { key: 'ArrowLeft', shiftKey: true });
    await fireEvent.keyDown(timeline, { key: 'Home' });
    await fireEvent.keyDown(timeline, { key: 'End' });

    expect(onSeek.mock.calls.map(([value]) => value)).toEqual([6, 0, 0, 10]);
    expect(timeline).toHaveAttribute(
      'aria-valuetext',
      en['waveform.position']
        .replace('{current}', isolate('5.0'))
        .replace('{duration}', isolate('10.0')),
    );
  });
});
