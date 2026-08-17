import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import PanelSplitter from '../../src/lib/PanelSplitter.svelte';

describe('PanelSplitter keyboard resizing', () => {
  afterEach(cleanup);

  it('exposes separator semantics and resizes a width with arrow keys', async () => {
    const onResize = vi.fn();
    render(PanelSplitter, {
      direction: 'horizontal',
      label: 'Resize library',
      onResize,
      value: 288,
    });
    const separator = screen.getByRole('separator', { name: 'Resize library' });

    expect(separator).toHaveAttribute('aria-orientation', 'vertical');
    expect(separator).toHaveAttribute('aria-valuemin', '200');
    expect(separator).toHaveAttribute('aria-valuemax', '600');
    expect(separator).toHaveAttribute('aria-valuenow', '288');
    await fireEvent.keyDown(separator, { key: 'ArrowLeft' });
    await fireEvent.keyDown(separator, { key: 'ArrowRight', shiftKey: true });
    await fireEvent.keyDown(separator, { key: 'ArrowDown' });

    expect(onResize.mock.calls.map(([delta]) => delta)).toEqual([-16, 48]);
  });
});
