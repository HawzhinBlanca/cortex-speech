import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/svelte';
import { invoke } from '@tauri-apps/api/core';
import { get } from 'svelte/store';
import AudioPlayer from '../../src/lib/AudioPlayer.svelte';
import { notifications } from '../../src/lib/stores/notificationStore';
import { locale } from '../../src/lib/i18n';

const invokeMock = vi.mocked(invoke);

function mockMediaResolution(path = 'C:\\media\\sample.wav') {
  invokeMock.mockImplementation(<T>(command: string, args?: unknown): Promise<T> => {
    const invokeArgs = args as { audioPath?: unknown } | undefined;
    if (command === 'register_media_asset') {
      return Promise.resolve({
        id: 'grant-1',
        path: invokeArgs?.audioPath,
        expiresAt: '2099-01-01T00:00:00Z',
      } as T);
    }
    if (command === 'get_media_asset_url') {
      return Promise.resolve(path as T);
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
    mockMediaResolution('C:\\media\\sample.wav');

    render(AudioPlayer, { props: { audioPath: 'C:\\input\\sample.wav' } });
    const audio = document.querySelector('audio') as HTMLAudioElement;

    await waitFor(() => {
      expect(audio.src).toContain('asset://localhost/C:/media/sample.wav');
    });
    expect(loadMock).toHaveBeenCalledOnce();
    expect(invokeMock).toHaveBeenCalledWith('register_media_asset', { audioPath: 'C:\\input\\sample.wav' });
    expect(invokeMock).toHaveBeenCalledWith('get_media_asset_url', { id: 'grant-1' });

    Object.defineProperty(audio, 'duration', { configurable: true, value: 42 });
    await fireEvent.loadedMetadata(audio);

    expect(screen.getByRole('button', { name: 'Play' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Playback Speed' })).toHaveAttribute('type', 'button');
    expect(screen.getByRole('button', { name: 'Toggle Loop Playback' })).toHaveTextContent('Loop Off');
  });

  it('shows a retry path when media registration fails', async () => {
    invokeMock.mockRejectedValueOnce(new Error('media grant failed'));

    render(AudioPlayer, { props: { audioPath: 'C:\\input\\broken.wav' } });

    // The component deliberately surfaces a clean, consistent message (the raw error
    // stays in the console) — see AudioPlayer.svelte error handling.
    await waitFor(() => {
      expect(screen.getByText('Failed to load audio file')).toBeInTheDocument();
    });

    mockMediaResolution('C:\\media\\recovered.wav');
    await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));

    const audio = document.querySelector('audio') as HTMLAudioElement;
    await waitFor(() => {
      expect(audio.src).toContain('asset://localhost/C:/media/recovered.wav');
    });
  });

  it('starts playback from clip start when current time is outside clip bounds', async () => {
    mockMediaResolution('C:\\media\\clip.wav');
    render(AudioPlayer, {
      props: {
        audioPath: 'C:\\input\\clip.wav',
        startTime: 10,
        endTime: 20,
      },
    });

    const audio = document.querySelector('audio') as HTMLAudioElement;
    await waitFor(() => {
      expect(audio.src).toContain('asset://localhost/C:/media/clip.wav');
    });
    Object.defineProperty(audio, 'duration', { configurable: true, value: 30 });
    await fireEvent.loadedMetadata(audio);
    audio.currentTime = 0;

    await fireEvent.click(screen.getByRole('button', { name: 'Play' }));

    expect(audio.currentTime).toBe(10);
    expect(playMock).toHaveBeenCalled();
  });

  it('shows a clip-relative scrubber (0 → clip length), not the whole-file duration', async () => {
    mockMediaResolution('C:\\media\\clip.wav');
    render(AudioPlayer, {
      props: { audioPath: 'C:\\input\\clip.wav', startTime: 10, endTime: 20 },
    });
    const audio = document.querySelector('audio') as HTMLAudioElement;
    await waitFor(() => expect(audio.src).toContain('asset://localhost/C:/media/clip.wav'));
    Object.defineProperty(audio, 'duration', { configurable: true, value: 30 });
    await fireEvent.loadedMetadata(audio);
    // The slider spans the 10s clip window, never the 30s source file.
    const slider = document.querySelector('input[type="range"]') as HTMLInputElement;
    expect(slider.max).toBe('10');
    // Total-time read-out shows the clip length (0:10), not the file length (0:30).
    expect(screen.getByText('0:10')).toBeInTheDocument();
    expect(screen.queryByText('0:30')).not.toBeInTheDocument();
  });

  it('reports loop replay failures instead of silently swallowing them', async () => {
    mockMediaResolution('C:\\media\\clip.wav');
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
      expect(audio.src).toContain('asset://localhost/C:/media/clip.wav');
    });
    Object.defineProperty(audio, 'duration', { configurable: true, value: 30 });
    await fireEvent.loadedMetadata(audio);
    await fireEvent.click(screen.getByRole('button', { name: 'Toggle Loop Playback' }));
    audio.currentTime = 20;

    await fireEvent.timeUpdate(audio);

    await waitFor(() => {
      expect(screen.getByText('Loop playback failed')).toBeInTheDocument();
    });
    const state = get(notifications);
    expect(state.some((item) => item.type === 'error' && item.message === 'Loop playback failed')).toBe(true);
  });
});
