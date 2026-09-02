import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const keyboard = vi.hoisted(() => ({
  manager: null as null | {
    getAll: () => Array<{
      key: string;
      ctrl?: boolean;
      shift?: boolean;
      alt?: boolean;
      description: string;
      descriptionKey?: string;
      action: () => void;
      category: string;
      allowInReview?: boolean;
    }>;
    formatShortcut: (shortcut: { key: string; ctrl?: boolean }) => string;
  },
}));

vi.mock('../../src/lib/keyboard', () => ({
  get globalKeyboardManager() {
    return keyboard.manager;
  },
}));

import CommandPalette from '../../src/lib/CommandPalette.svelte';
import { locale } from '../../src/lib/i18n';

beforeEach(() => {
  locale.set('en');
  keyboard.manager = null;
  Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', {
    configurable: true,
    value: vi.fn(),
  });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('CommandPalette', () => {
  it('is inert while closed and renders a truthful empty state without a keyboard manager', async () => {
    const { rerender } = render(CommandPalette, { props: { open: false } });
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();

    await rerender({ open: true });
    expect(screen.getByRole('combobox')).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText('No matching commands')).toBeInTheDocument();
    expect(screen.getByText('0 commands')).toBeInTheDocument();
    expect(screen.getByRole('combobox')).not.toHaveAttribute('aria-activedescendant');

    await fireEvent.keyDown(screen.getByRole('combobox'), { key: 'ArrowDown' });
    await fireEvent.keyDown(screen.getByRole('combobox'), { key: 'ArrowUp' });
    await fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Enter' });
  });

  it('maps shortcut labels/categories/hints and excludes non-review-safe globals', () => {
    keyboard.manager = {
      getAll: () => [
        {
          key: 'o',
          ctrl: true,
          description: 'legacy open',
          descriptionKey: 'openFile',
          action: vi.fn(),
          category: 'file',
          allowInReview: true,
        },
        {
          key: 'x',
          description: 'Unsafe curate action',
          action: vi.fn(),
          category: 'owner custom',
          allowInReview: false,
        },
      ],
      formatShortcut: (shortcut) =>
        shortcut.ctrl ? `Ctrl+${shortcut.key.toUpperCase()}` : shortcut.key,
    };

    render(CommandPalette, { props: { open: true, reviewActive: true } });
    expect(screen.getByRole('option', { name: /Add file/ })).toHaveTextContent('File Operations');
    expect(screen.getByText('Ctrl+O')).toBeInTheDocument();
    expect(screen.queryByText('Unsafe curate action')).not.toBeInTheDocument();
  });

  it('filters by fuzzy label and category with case-insensitive input', async () => {
    const commands = [
      { id: 'import', label: 'Import export', category: 'File', run: vi.fn() },
      { id: 'health', label: 'Open health', category: 'Diagnostics', run: vi.fn() },
      { id: 'review', label: 'Review queue', category: 'Owner flow', run: vi.fn() },
    ];
    render(CommandPalette, { props: { open: true, extraCommands: commands } });
    const input = screen.getByRole('combobox');

    await fireEvent.input(input, { target: { value: 'imex' } });
    expect(screen.getAllByRole('option')).toHaveLength(1);
    expect(screen.getByText('Import export')).toBeInTheDocument();

    await fireEvent.input(input, { target: { value: 'DIAGNOSTICS' } });
    expect(screen.getAllByRole('option')).toHaveLength(1);
    expect(screen.getByText('Open health')).toBeInTheDocument();

    await fireEvent.input(input, { target: { value: 'queue' } });
    expect(screen.getAllByRole('option')).toHaveLength(1);
    expect(screen.getByText('Review queue')).toBeInTheDocument();

    await fireEvent.input(input, { target: { value: 'no result' } });
    expect(screen.getByText('No matching commands')).toBeInTheDocument();
  });

  it('wraps keyboard selection, follows pointer selection, and runs only after modal close', async () => {
    vi.useFakeTimers();
    const first = vi.fn();
    const second = vi.fn();
    const onClose = vi.fn();
    render(CommandPalette, {
      props: {
        open: true,
        onClose,
        extraCommands: [
          { id: 'first', label: 'First', category: 'One', run: first },
          { id: 'second', label: 'Second', category: 'Two', hint: 'S', run: second },
        ],
      },
    });
    const input = screen.getByRole('combobox');
    const options = screen.getAllByRole('option');
    expect(options[0]).toHaveAttribute('aria-selected', 'true');

    await fireEvent.keyDown(input, { key: 'ArrowUp' });
    expect(options[1]).toHaveAttribute('aria-selected', 'true');
    await fireEvent.keyDown(input, { key: 'ArrowDown' });
    expect(options[0]).toHaveAttribute('aria-selected', 'true');
    await fireEvent.mouseEnter(options[1]);
    expect(options[1]).toHaveAttribute('aria-selected', 'true');

    await fireEvent.keyDown(input, { key: 'Enter' });
    expect(onClose).toHaveBeenCalledOnce();
    expect(second).not.toHaveBeenCalled();
    await vi.runAllTimersAsync();
    expect(second).toHaveBeenCalledOnce();

    await fireEvent.click(options[0]);
    await vi.runAllTimersAsync();
    expect(first).toHaveBeenCalledOnce();
  });
});
