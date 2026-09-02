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

    // "hello world" -> "hello beautiful world" is a pure INSERTION of "beautiful". It must render as an
    // inserted word, NOT the misleading "world \u2192 beautiful" substitution the old LCS reconstruction
    // emitted (it consumed the unchanged "world" into a bogus replace).
    expect(await screen.findByText('beautiful')).toBeInTheDocument();
    expect(screen.queryByText('world \u2192 beautiful')).not.toBeInTheDocument();
    await waitFor(() => expect(invokeMock).not.toHaveBeenCalled());
  });

  it('surfaces an authoritative typed desktop refusal without retrying the diff locally', async () => {
    window.__TAURI__ = {};
    invokeMock.mockRejectedValueOnce({
      schema: 1,
      code: 'DIFF_TOO_COMPLEX',
      message: 'private backend text must not render',
      retryable: false,
      suggestedAction: null,
      operationId: null,
    });

    render(DiffView, {
      props: {
        raw: 'hello world',
        annotated: 'goodbye world',
      },
    });

    expect(await screen.findByText(/DIFF_TOO_COMPLEX/)).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith('compute_diff', {
      raw: 'hello world',
      annotated: 'goodbye world',
    });
    expect(screen.queryByText('hello \u2192 goodbye')).not.toBeInTheDocument();
    expect(screen.queryByText(/private backend text/)).not.toBeInTheDocument();
  });
});
