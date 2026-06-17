import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import DiffView from '../../src/lib/DiffView.svelte';
import { locale } from '../../src/lib/i18n';

const invokeMock = vi.mocked(invoke);

describe('DiffView runtime behavior', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    delete window.__TAURI__;
    delete window.__TAURI_INTERNALS__;
    locale.set('en');
  });

  afterEach(() => {
    cleanup();
  });

  it('computes diffs locally in browser mode without invoking Tauri', async () => {
    render(DiffView, {
      props: {
        raw: 'hello world',
        annotated: 'hello beautiful world',
      },
    });

    expect(await screen.findByText('world \u2192 beautiful')).toBeInTheDocument();
    await waitFor(() => expect(invokeMock).not.toHaveBeenCalled());
  });

  it('falls back to the local diff if the Tauri command rejects', async () => {
    window.__TAURI__ = {};
    invokeMock.mockRejectedValueOnce(new Error('diff backend unavailable'));

    render(DiffView, {
      props: {
        raw: 'hello world',
        annotated: 'goodbye world',
      },
    });

    expect(await screen.findByText('hello \u2192 goodbye')).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith('compute_diff', {
      raw: 'hello world',
      annotated: 'goodbye world',
    });
    expect(screen.queryByText(/diff backend unavailable/)).not.toBeInTheDocument();
  });
});
