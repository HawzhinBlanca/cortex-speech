import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import LazyFixture from '../../tests/fixtures/LazyFixture.svelte';
import LazyComponent from './LazyComponent.svelte';

const labels = {
  loadingLabel: 'Loading workspace',
  failedLabel: 'Workspace failed',
  retryLabel: 'Retry workspace',
  closeLabel: 'Close workspace',
};

describe('LazyComponent', () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it('shows a localized pending state and mounts the loaded component with props', async () => {
    let resolveModule: ((module: unknown) => void) | undefined;
    const load = vi.fn(
      () =>
        new Promise<unknown>((resolve) => {
          resolveModule = resolve;
        }),
    );

    render(LazyComponent, {
      props: {
        load,
        componentProps: { message: 'settings ready' },
        ...labels,
      },
    });

    expect(load).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('status')).toHaveTextContent('Loading workspace');

    resolveModule?.({ default: LazyFixture });
    expect(await screen.findByText('settings ready')).toBeInTheDocument();
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
  });

  it('hides raw loader errors, then retries without remounting the parent', async () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    const load = vi
      .fn<() => Promise<unknown>>()
      .mockRejectedValueOnce(new Error('C:/private/user/path was not found'))
      .mockResolvedValueOnce({ default: LazyFixture });
    const onClose = vi.fn();

    render(LazyComponent, {
      props: { load, onClose, ...labels },
    });

    expect(await screen.findByRole('alert')).toHaveTextContent('Workspace failed');
    expect(screen.queryByText(/private\/user\/path/)).not.toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: 'Retry workspace' }));
    expect(await screen.findByTestId('lazy-fixture')).toBeInTheDocument();
    expect(load).toHaveBeenCalledTimes(2);
    expect(onClose).not.toHaveBeenCalled();
  });
});
