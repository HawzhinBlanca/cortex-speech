import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const runtime = vi.hoisted(() => ({ available: true }));
const api = vi.hoisted(() => ({
  modelsStatus: vi.fn(),
  modelsDownloadAll: vi.fn(),
}));
const desktopEvents = vi.hoisted(() => ({
  handler: null as null | ((event: { payload: unknown }) => void),
  unlisten: vi.fn(),
  subscribe: vi.fn((_event: string, handler: (event: { payload: unknown }) => void) => {
    desktopEvents.handler = handler;
    return Promise.resolve(desktopEvents.unlisten);
  }),
}));

vi.mock('../../src/lib/runtime', () => ({
  isTauriRuntime: () => runtime.available,
}));

vi.mock('../../src/lib/commands', () => ({
  modelsStatus: api.modelsStatus,
  modelsDownloadAll: api.modelsDownloadAll,
}));

vi.mock('../../src/lib/events', () => ({
  subscribeDesktopEvent: desktopEvents.subscribe,
}));

import ModelDownload from '../../src/lib/ModelDownload.svelte';
import { locale } from '../../src/lib/i18n';
import { notifications } from '../../src/lib/stores/notificationStore';

const MODELS = [
  {
    name: 'Owner champion',
    filename: 'champion.bin',
    downloaded: true,
    exists: true,
    sizeBytes: 2_097_152,
    minSizeBytes: 1,
    version: '1',
    source: 'user',
    downloadable: true,
  },
  {
    name: 'Optional helper',
    filename: 'helper.bin',
    downloaded: false,
    exists: false,
    sizeBytes: null,
    minSizeBytes: 1,
    version: '1',
    source: 'missing',
    downloadable: false,
  },
] as const;

function emit(payload: unknown): void {
  expect(desktopEvents.handler).toBeTypeOf('function');
  desktopEvents.handler?.({ payload });
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

beforeEach(() => {
  locale.set('en');
  runtime.available = true;
  api.modelsStatus.mockReset().mockResolvedValue(MODELS);
  api.modelsDownloadAll.mockReset().mockResolvedValue({
    downloaded: 1,
    failed: 0,
    total: 1,
    skipped: 0,
  });
  desktopEvents.handler = null;
  desktopEvents.subscribe.mockClear();
  desktopEvents.unlisten.mockReset();
  notifications.clear();
});

afterEach(() => {
  cleanup();
  notifications.clear();
  vi.restoreAllMocks();
});

describe('ModelDownload', () => {
  it('fails closed outside desktop without querying or subscribing to native state', async () => {
    runtime.available = false;
    render(ModelDownload);
    const button = screen.getByRole('button', { name: 'Download All' });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute('title', 'Desktop app runtime required');
    expect(api.modelsStatus).not.toHaveBeenCalled();
    expect(desktopEvents.subscribe).not.toHaveBeenCalled();
  });

  it('loads downloaded and missing rows with honest availability and byte-size copy', async () => {
    render(ModelDownload);
    expect(await screen.findByText('Owner champion')).toBeInTheDocument();
    expect(screen.getByText('Optional helper')).toBeInTheDocument();
    expect(screen.getByText('2.0 MB')).toBeInTheDocument();
    expect(screen.getAllByText('Not downloaded').length).toBeGreaterThanOrEqual(2);
    expect(desktopEvents.subscribe).toHaveBeenCalledWith(
      'model-download-progress',
      expect.any(Function),
    );
  });

  it('surfaces status-read failure without leaving the skeleton stuck', async () => {
    api.modelsStatus.mockRejectedValueOnce(new Error('registry unavailable'));
    render(ModelDownload);
    await vi.waitFor(() => expect(get(notifications).at(-1)).toMatchObject({ type: 'error' }));
    expect(screen.queryByText('Owner champion')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Download All' })).toBeEnabled();
  });

  it.each([
    [
      'missing verified pins',
      { downloaded: 0, failed: 0, total: 0, skipped: 2 },
      'info',
      'No verified model downloads available',
    ],
    [
      'partial failures',
      { downloaded: 1, failed: 2, total: 3, skipped: 0 },
      'warning',
      'Model download completed with failures',
    ],
    [
      'successful skips',
      { downloaded: 1, failed: 0, total: 1, skipped: 1 },
      'success',
      'Verified model downloads completed',
    ],
    [
      'complete success',
      { downloaded: 1, failed: 0, total: 1, skipped: 0 },
      'success',
      'Verified model downloads completed',
    ],
  ] as const)('reports %s from the exact native summary', async (_name, result, type, message) => {
    api.modelsDownloadAll.mockResolvedValueOnce(result);
    render(ModelDownload);
    const button = await screen.findByRole('button', { name: 'Download All' });
    await fireEvent.click(button);
    await vi.waitFor(() =>
      expect(
        get(notifications).some((notice) => notice.type === type && notice.message === message),
      ).toBe(true),
    );
    expect(api.modelsStatus.mock.calls.length).toBeGreaterThanOrEqual(2);
    await vi.waitFor(() =>
      expect(screen.getByRole('button', { name: 'Download All' })).toBeEnabled(),
    );
  });

  it('contains a rejected download and restores an actionable idle button', async () => {
    api.modelsDownloadAll.mockRejectedValueOnce(new Error('download refused'));
    render(ModelDownload);
    await fireEvent.click(await screen.findByRole('button', { name: 'Download All' }));
    await vi.waitFor(() => expect(get(notifications).at(-1)).toMatchObject({ type: 'error' }));
    expect(screen.getByRole('button', { name: 'Download All' })).toBeEnabled();
  });

  it('renders started/progress events, tracks named model progress, and refreshes on completion', async () => {
    const download = deferred<{
      downloaded: number;
      failed: number;
      total: number;
      skipped: number;
    }>();
    api.modelsDownloadAll.mockReturnValueOnce(download.promise);
    render(ModelDownload);
    await screen.findByText('Owner champion');
    await fireEvent.click(screen.getByRole('button', { name: 'Download All' }));
    emit({ type: 'started', total: 4 });
    await vi.waitFor(() => expect(screen.getAllByText('Downloading...').length).toBeGreaterThan(1));
    emit({
      type: 'progress',
      current: 1,
      status: '',
      filename: 'champion.bin',
      progress: 0.25,
    });
    emit({ type: 'progress', current: 2, status: 'Downloading helper', progress: 0.5 });
    emit({
      type: 'progress',
      current: 3,
      status: 'Verifying helper',
      filename: 'helper.bin',
      progress: 0.75,
    });

    await vi.waitFor(() => expect(screen.getByText('Verifying helper')).toBeInTheDocument());
    expect(screen.getByText('3 / 4')).toBeInTheDocument();
    expect(document.querySelector('[style="width: 75%;"]')).toBeTruthy();

    const callsBefore = api.modelsStatus.mock.calls.length;
    emit({ type: 'completed' });
    download.resolve({ downloaded: 1, failed: 0, total: 1, skipped: 0 });
    await vi.waitFor(() => expect(api.modelsStatus.mock.calls.length).toBeGreaterThan(callsBefore));
  });

  it('unsubscribes the exact desktop event listener on teardown', async () => {
    const view = render(ModelDownload);
    await screen.findByText('Owner champion');
    view.unmount();
    await vi.waitFor(() => expect(desktopEvents.unlisten).toHaveBeenCalledOnce());
  });
});
