import { describe, it, expect, beforeEach, vi } from 'vitest';
import { listen } from '@tauri-apps/api/event';
import { isTauriRuntime } from '../../src/lib/runtime';
import { startEventListeners, stopEventListeners } from '../../src/lib/events';

const listenMock = vi.mocked(listen);

describe('runtime detection', () => {
  beforeEach(() => {
    delete window.__TAURI__;
    delete window.__TAURI_INTERNALS__;
    listenMock.mockClear();
    stopEventListeners();
  });

  it('returns false in a plain browser environment', () => {
    expect(isTauriRuntime()).toBe(false);
  });

  it('returns true when Tauri internals are present', () => {
    window.__TAURI_INTERNALS__ = {};
    expect(isTauriRuntime()).toBe(true);
  });

  it('does not register Tauri event listeners outside Tauri', async () => {
    await startEventListeners();
    expect(listenMock).not.toHaveBeenCalled();
  });
});
