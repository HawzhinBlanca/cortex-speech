import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import DatasetMerge from '../../src/lib/DatasetMerge.svelte';
import { setLocale } from '../../src/lib/i18n';
import { notifications } from '../../src/lib/stores/notificationStore';
import { segments } from '../../src/lib/stores/segmentStore';
import { showDatasetMerge } from '../../src/lib/stores/uiStore';

const invokeMock = vi.mocked(invoke);

beforeEach(async () => {
  invokeMock.mockReset();
  notifications.clear();
  showDatasetMerge.set(true);
  await setLocale('en');
  vi.spyOn(segments, 'load').mockResolvedValue(true);
});

afterEach(() => {
  cleanup();
  notifications.clear();
  showDatasetMerge.set(false);
  vi.restoreAllMocks();
});

describe('DatasetMerge explicit JSON workflow', () => {
  it('exposes a bidi-safe JSON field and refuses blank content before IPC', async () => {
    render(DatasetMerge);

    expect(screen.getByRole('dialog')).toHaveAccessibleName('Merge Dataset');
    const input = screen.getByRole('textbox', { name: 'Dataset JSON' });
    expect(input).toHaveAttribute('dir', 'ltr');
    expect(input).toHaveAttribute('spellcheck', 'false');

    await fireEvent.click(screen.getByRole('button', { name: 'Merge Dataset' }));
    expect(invokeMock).not.toHaveBeenCalled();
    expect(get(notifications).at(-1)).toMatchObject({
      type: 'error',
      message: 'Please paste dataset JSON content',
    });

    await fireEvent.input(input, { target: { value: '   ' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Merge Dataset' }));
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('submits trimmed JSON once, locks controls while pending, and reloads after success', async () => {
    let resolveMerge:
      ((value: { created: number; updated: number; conflicts: number }) => void) | undefined;
    invokeMock.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveMerge = resolve;
        }),
    );
    render(DatasetMerge);
    const input = screen.getByRole('textbox', { name: 'Dataset JSON' });
    await fireEvent.input(input, { target: { value: '  {"segments":[]}\n' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Merge Dataset' }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith('merge_dataset_json', {
        jsonContent: '{"segments":[]}',
      }),
    );
    expect(screen.getByRole('button', { name: 'Merging...' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeDisabled();
    expect(invokeMock).toHaveBeenCalledOnce();

    resolveMerge?.({ created: 2, updated: 3, conflicts: 0 });
    await waitFor(() => expect(get(showDatasetMerge)).toBe(false));
    expect(get(notifications).at(-1)).toMatchObject({
      type: 'success',
      message: 'Merge complete: 2 created, 3 updated',
    });
    expect(input).toHaveValue('');
    expect(segments.load).toHaveBeenCalledOnce();
    expect(screen.getByRole('button', { name: 'Merge Dataset' })).not.toBeDisabled();
  });

  it('retains the exact JSON after a typed failure and re-enables retry', async () => {
    invokeMock.mockRejectedValue({
      schema: 1,
      code: 'MERGE_REJECTED',
      message: 'private row detail',
      retryable: false,
      suggestedAction: 'openHealth',
      operationId: 'merge-op-1',
    });
    render(DatasetMerge);
    const input = screen.getByRole('textbox', { name: 'Dataset JSON' });
    await fireEvent.input(input, { target: { value: '{"owner":"verbatim"}' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Merge Dataset' }));

    await waitFor(() =>
      expect(get(notifications).at(-1)).toMatchObject({
        type: 'error',
        message: 'Merge failed',
        suggestedAction: 'openHealth',
        retryable: false,
      }),
    );
    expect(input).toHaveValue('{"owner":"verbatim"}');
    expect(get(showDatasetMerge)).toBe(true);
    expect(segments.load).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Merge Dataset' })).not.toBeDisabled();
  });

  it('closes from Escape, backdrop, header, and cancel while idle', async () => {
    render(DatasetMerge);

    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    expect(get(showDatasetMerge)).toBe(false);
    showDatasetMerge.set(true);
    await fireEvent.click(screen.getByRole('dialog'));
    expect(get(showDatasetMerge)).toBe(false);
    showDatasetMerge.set(true);
    await fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(get(showDatasetMerge)).toBe(false);
    showDatasetMerge.set(true);
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(get(showDatasetMerge)).toBe(false);
  });
});
