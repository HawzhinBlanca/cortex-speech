import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/svelte';
import { invoke } from '@tauri-apps/api/core';
import { get } from 'svelte/store';
import AudioPlayer from '../../src/lib/AudioPlayer.svelte';
import { notifications } from '../../src/lib/stores/notificationStore';
import { locale } from '../../src/lib/i18n';
import { ckb } from '../../src/lib/i18n/ckb';

const invokeMock = vi.mocked(invoke);
const MEDIA_GRANT_ID = '2f2d9b66-8566-4d1c-8c14-e18d006b776f';
const MEDIA_URL = `http://cortex-media.localhost/${MEDIA_GRANT_ID}`;

function mockMediaResolution() {
  invokeMock.mockImplementation(<T>(command: string, args?: unknown): Promise<T> => {
    if (command === 'register_media_asset') {
      return Promise.resolve({
        id: MEDIA_GRANT_ID,
        expiresAt: '2099-01-01T00:00:00Z',
      } as T);
    }
    if (command === 'get_media_asset_url') {
      return Promise.resolve(MEDIA_URL as T);
    }
    return Promise.reject(new Error(`Unexpected command: ${command}`));
  });
}

describe('AudioPlayer', () => {
  let loadMock: ReturnType<typeof vi.fn>;
  let playMock: ReturnType<typeof vi.fn>;
  let pauseMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    // The error + retry copy is i18n; pin English here (app defaults to Sorani) and
    // restore the default in afterEach so other files' Sorani assertions are unaffected.
    locale.set('en');
    invokeMock.mockReset();
    loadMock = vi.fn();
    playMock = vi.fn(() => Promise.resolve());
    pauseMock = vi.fn();
    Object.defineProperty(HTMLMediaElement.prototype, 'load', {
      configurable: true,
      value: loadMock,
    });
    Object.defineProperty(HTMLMediaElement.prototype, 'play', {
      configurable: true,
      value: playMock,
    });
    Object.defineProperty(HTMLMediaElement.prototype, 'pause', {
      configurable: true,
      value: pauseMock,
    });
    notifications.clear();
  });

  afterEach(() => {
    locale.set('ckb');
    vi.restoreAllMocks();
  });

  it('registers and loads a granted media asset before showing controls', async () => {
    mockMediaResolution();

    render(AudioPlayer, { props: { audioPath: 'C:\\input\\sample.wav' } });
    const audio = document.querySelector('audio') as HTMLAudioElement;

    await waitFor(() => {
      expect(audio.src).toBe(MEDIA_URL);
    });
    expect(loadMock).toHaveBeenCalledOnce();
    expect(invokeMock).toHaveBeenCalledWith('register_media_asset', {
      audioPath: 'C:\\input\\sample.wav',
    });
    expect(invokeMock).toHaveBeenCalledWith('get_media_asset_url', { id: MEDIA_GRANT_ID });

    Object.defineProperty(audio, 'duration', { configurable: true, value: 42 });
    await fireEvent.loadedMetadata(audio);

    expect(screen.getByRole('button', { name: 'Play' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Playback Speed' })).toHaveAttribute(
      'type',
      'button',
    );
    expect(screen.getByRole('button', { name: 'Toggle Loop Playback' })).toHaveTextContent(
      'Loop Off',
    );
  });

  it('gives the seek slider a localized name and uses a wrap-safe control layout', async () => {
    mockMediaResolution();
    render(AudioPlayer, { props: { audioPath: 'C:\\input\\sample.wav' } });
    const audio = document.querySelector('audio') as HTMLAudioElement;
    await waitFor(() => expect(audio.src).toBe(MEDIA_URL));
    Object.defineProperty(audio, 'duration', { configurable: true, value: 42 });
    await fireEvent.loadedMetadata(audio);

    const controls = screen.getByTestId('audio-player-controls');
    expect(controls).toHaveClass('flex-wrap');
    expect(screen.getByTestId('audio-player-timeline')).toHaveClass('min-w-0', 'flex-1');
    expect(screen.getByRole('slider', { name: 'Seek audio' })).toHaveClass('min-w-0');

    locale.set('ckb');
    await waitFor(() => {
      expect(screen.getByRole('toolbar', { name: ckb['audio.controls'] })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: ckb['audio.play'] })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: ckb['audio.playbackSpeed'] })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: ckb['audio.loopToggle'] })).toHaveTextContent(
        ckb['audio.loopOff'],
      );
      expect(screen.getByRole('slider', { name: ckb['audio.seek'] })).toBeInTheDocument();
    });
  });

  it('shows a retry path when media registration fails', async () => {
    invokeMock.mockRejectedValueOnce(new Error('media grant failed'));

    render(AudioPlayer, { props: { audioPath: 'C:\\input\\broken.wav' } });

    // The component deliberately surfaces a clean, consistent message (the raw error
    // stays in the console) — see AudioPlayer.svelte error handling.
    await waitFor(() => {
      expect(screen.getByText('Failed to load audio file')).toBeInTheDocument();
    });

    mockMediaResolution();
    await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));

    const audio = document.querySelector('audio') as HTMLAudioElement;
    await waitFor(() => {
      expect(audio.src).toBe(MEDIA_URL);
    });
  });

  it('starts playback from clip start when current time is outside clip bounds', async () => {
    mockMediaResolution();
    render(AudioPlayer, {
      props: {
        audioPath: 'C:\\input\\clip.wav',
        startTime: 10,
        endTime: 20,
      },
    });

    const audio = document.querySelector('audio') as HTMLAudioElement;
    await waitFor(() => {
      expect(audio.src).toBe(MEDIA_URL);
    });
    Object.defineProperty(audio, 'duration', { configurable: true, value: 30 });
    await fireEvent.loadedMetadata(audio);
    audio.currentTime = 0;

    await fireEvent.click(screen.getByRole('button', { name: 'Play' }));

    expect(audio.currentTime).toBe(10);
    expect(playMock).toHaveBeenCalled();
  });

  it('shows a clip-relative scrubber (0 → clip length), not the whole-file duration', async () => {
    mockMediaResolution();
    render(AudioPlayer, {
      props: { audioPath: 'C:\\input\\clip.wav', startTime: 10, endTime: 20 },
    });
    const audio = document.querySelector('audio') as HTMLAudioElement;
    await waitFor(() => expect(audio.src).toBe(MEDIA_URL));
    Object.defineProperty(audio, 'duration', { configurable: true, value: 30 });
    await fireEvent.loadedMetadata(audio);
    // The slider spans the 10s clip window, never the 30s source file.
    const slider = document.querySelector('input[type="range"]') as HTMLInputElement;
    expect(slider.max).toBe('10');
    // Total-time read-out shows the clip length (0:10), not the file length (0:30).
    expect(screen.getByText('0:10')).toBeInTheDocument();
    expect(screen.queryByText('0:30')).not.toBeInTheDocument();
  });

  it('reschedules the precise clip stop when endTime is retargeted mid-play (tap-a-word)', async () => {
    mockMediaResolution();
    const { rerender } = render(AudioPlayer, {
      props: { audioPath: 'C:\\input\\clip.wav', startTime: 10, endTime: 20 },
    });
    const audio = document.querySelector('audio') as HTMLAudioElement;
    await waitFor(() => expect(audio.src).toBe(MEDIA_URL));
    Object.defineProperty(audio, 'duration', { configurable: true, value: 30 });
    await fireEvent.loadedMetadata(audio);
    audio.currentTime = 10.5;

    // The element is actually playing (mocked play() doesn't flip the native paused getter, and
    // scheduleClipStop correctly bails while paused — so reflect a truly-playing element).
    Object.defineProperty(audio, 'paused', { configurable: true, value: false });

    vi.useFakeTimers();
    try {
      await fireEvent.click(screen.getByRole('button', { name: 'Play' }));
      await Promise.resolve(); // flush play()'s .then so playing=true + the stop timer arm
      expect(playMock).toHaveBeenCalled();

      // A parent retargets the window mid-play (tap-a-word): the stop must move from the old
      // 20s boundary (9.5s away) to the word end (1s away) — without this the word bleeds on.
      await rerender({ endTime: 11.5 });
      vi.advanceTimersByTime(1100);
      expect(pauseMock).toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it('reports loop replay failures instead of silently swallowing them', async () => {
    locale.set('ckb');
    mockMediaResolution();
    playMock.mockRejectedValueOnce(new Error('autoplay denied'));
    render(AudioPlayer, {
      props: {
        audioPath: 'C:\\input\\clip.wav',
        startTime: 10,
        endTime: 20,
      },
    });

    const audio = document.querySelector('audio') as HTMLAudioElement;
    await waitFor(() => {
      expect(audio.src).toBe(MEDIA_URL);
    });
    Object.defineProperty(audio, 'duration', { configurable: true, value: 30 });
    await fireEvent.loadedMetadata(audio);
    await fireEvent.click(screen.getByRole('button', { name: ckb['audio.loopToggle'] }));
    audio.currentTime = 20;

    await fireEvent.timeUpdate(audio);

    await waitFor(() => {
      expect(screen.getByText(ckb['audio.loopFailed'])).toBeInTheDocument();
    });
    const state = get(notifications);
    expect(
      state.some((item) => item.type === 'error' && item.message === ckb['audio.loopFailed']),
    ).toBe(true);
  });
});
