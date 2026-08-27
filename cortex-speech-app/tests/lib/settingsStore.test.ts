import { describe, it, expect, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import { settings, defaultSettings } from '../../src/lib/stores/settingsStore';

describe('settingsStore', () => {
  beforeEach(() => {
    settings.set({ ...defaultSettings });
  });

  it('has defaults', () => {
    const state = get(settings);
    expect(state.theme).toBe('dark');
    expect(state.numThreads).toBe(4);
    expect(state.enableGpu).toBe(true);
    expect(state.asrModel).toBe('wsl-7b');
    expect(state.language).toBe('ckb');
    expect(state.sourceReferenceModels).toEqual(['gemini-2.5-pro']);
  });

  it('allows direct update', () => {
    settings.set({ ...get(settings), numThreads: 8 });
    expect(get(settings).numThreads).toBe(8);
  });

  it('persists theme', () => {
    const s = get(settings);
    s.theme = 'light';
    settings.set(s);
    expect(get(settings).theme).toBe('light');
  });

  it('round-trips all fields', () => {
    const updated = {
      ...defaultSettings,
      theme: 'light' as const,
      autoNormalize: false,
      numThreads: 16,
      language: 'en',
    };
    settings.set(updated);
    expect(get(settings)).toEqual(updated);
  });
});
