import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/svelte';
import ConfirmDialog from '../../src/lib/ConfirmDialog.svelte';
import { showConfirmDialog } from '../../src/lib/stores/uiStore';
import { locale } from '../../src/lib/i18n';

describe('ConfirmDialog', () => {
  beforeEach(() => {
    locale.set('en');
    showConfirmDialog.set(null);
  });

  it('does not render when no dialog is set', () => {
    render(ConfirmDialog);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('renders the dialog with the message and buttons when set', async () => {
    render(ConfirmDialog);
    showConfirmDialog.set({
      title: 'Test',
      message: 'Are you sure?',
      onConfirm: vi.fn(),
    });

    await waitFor(() => {
      expect(screen.getByRole('dialog')).toBeInTheDocument();
    });
    expect(screen.getByText('Are you sure?')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Confirm' })).toBeInTheDocument();
    expect(screen.getByText('Cancel')).toBeInTheDocument();
  });

  it('calls onConfirm and closes the dialog when Confirm is clicked', async () => {
    render(ConfirmDialog);
    const onConfirm = vi.fn();
    showConfirmDialog.set({
      title: 'Test',
      message: 'Proceed?',
      onConfirm,
    });

    await waitFor(() => {
    expect(screen.getByRole('button', { name: 'Confirm' })).toBeInTheDocument();
    });

    await fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));

    expect(onConfirm).toHaveBeenCalledOnce();
    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
  });

  it('closes the dialog without calling onConfirm when Cancel is clicked', async () => {
    render(ConfirmDialog);
    const onConfirm = vi.fn();
    showConfirmDialog.set({
      title: 'Test',
      message: 'Cancel me?',
      onConfirm,
    });

    await waitFor(() => {
      expect(screen.getByText('Cancel')).toBeInTheDocument();
    });

    await fireEvent.click(screen.getByText('Cancel'));

    expect(onConfirm).not.toHaveBeenCalled();
    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
  });

  it('closes the dialog when the Escape key is pressed', async () => {
    render(ConfirmDialog);
    const onConfirm = vi.fn();
    showConfirmDialog.set({
      title: 'Test',
      message: 'Esc me?',
      onConfirm,
    });

    await waitFor(() => {
      expect(screen.getByRole('dialog')).toBeInTheDocument();
    });

    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });

    expect(onConfirm).not.toHaveBeenCalled();
    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
  });

  it('renders a secondary action with custom labels and fires only its handler', async () => {
    render(ConfirmDialog);
    const onConfirm = vi.fn();
    const onSecondary = vi.fn();
    showConfirmDialog.set({
      title: '7B down',
      message: 'Champion unavailable',
      confirmLabel: 'Try 7B again',
      danger: false,
      onConfirm,
      secondary: { label: 'Use offline model', onClick: onSecondary },
    });

    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Use offline model' })).toBeInTheDocument();
    });
    // Custom confirm label is used instead of the default 'Confirm'.
    expect(screen.getByRole('button', { name: 'Try 7B again' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Confirm' })).not.toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: 'Use offline model' }));

    expect(onSecondary).toHaveBeenCalledOnce();
    expect(onConfirm).not.toHaveBeenCalled();
    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
  });

  it('does not fire the destructive onConfirm on a stray Enter (Cancel is the safe default)', async () => {
    // By design (ConfirmDialog.svelte): Cancel is autofocused so a stray Enter
    // dismisses rather than firing the destructive action. Pin that safety contract —
    // a bare Enter on the dialog must never trigger onConfirm.
    render(ConfirmDialog);
    const onConfirm = vi.fn();
    showConfirmDialog.set({
      title: 'Test',
      message: 'Enter me?',
      onConfirm,
    });

    await waitFor(() => {
      expect(screen.getByRole('dialog')).toBeInTheDocument();
    });

    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Enter' });

    expect(onConfirm).not.toHaveBeenCalled();
  });
});
