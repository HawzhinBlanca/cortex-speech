import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import WorkstationRecoveryNotices from './WorkstationRecoveryNotices.svelte';
import { locale } from './i18n';
import { showConfirmDialog } from './stores/uiStore';

describe('WorkstationRecoveryNotices', () => {
  beforeEach(() => locale.set('en'));
  afterEach(() => {
    cleanup();
    showConfirmDialog.set(null);
    locale.set('ckb');
  });

  it('renders only the public quarantine count and wires explicit owner actions', async () => {
    const onAcknowledgeQuarantine = vi.fn();
    const onDismissQuarantine = vi.fn();
    render(WorkstationRecoveryNotices, {
      quarantineNotice: {
        quarantinedFileCount: 2,
        snapshotCount: 3,
        newestSnapshotSegments: 42,
      },
      interruptedImport: null,
      importRecoveryBusy: false,
      importRecoveryAuthority: 'known',
      workspaceOperationBusy: false,
      onAcknowledgeQuarantine,
      onDismissQuarantine,
      onResumeImport: vi.fn(),
      onDismissImport: vi.fn(),
      onRetryRecoveryCheck: vi.fn(),
    });

    const banner = screen.getByTestId('quarantine-banner');
    expect(banner).toHaveTextContent('2 file(s)');
    expect(banner).toHaveTextContent('3 auto-snapshot(s)');
    expect(banner).not.toHaveTextContent('cortex-speech.corrupt');

    await fireEvent.click(screen.getByTestId('acknowledge-quarantine-btn'));
    await fireEvent.click(screen.getByTestId('dismiss-quarantine-btn'));
    expect(onAcknowledgeQuarantine).toHaveBeenCalledOnce();
    expect(onDismissQuarantine).toHaveBeenCalledOnce();
  });

  it('renders path-free interrupted-import progress and wires resume and discard', async () => {
    const onResumeImport = vi.fn();
    const onDismissImport = vi.fn();
    render(WorkstationRecoveryNotices, {
      quarantineNotice: null,
      interruptedImport: {
        id: 'import-job-1',
        totalFiles: 19,
        completedCount: 7,
        createdAt: '2026-08-28T10:00:00Z',
      },
      importRecoveryBusy: false,
      importRecoveryAuthority: 'known',
      workspaceOperationBusy: false,
      onAcknowledgeQuarantine: vi.fn(),
      onDismissQuarantine: vi.fn(),
      onResumeImport,
      onDismissImport,
      onRetryRecoveryCheck: vi.fn(),
    });

    const banner = screen.getByTestId('resume-import-banner');
    expect(banner).toHaveTextContent('7/19 files done');
    expect(banner).not.toHaveTextContent('C:');
    expect(banner).toHaveClass('min-w-0', 'flex-col');
    expect(screen.getByTestId('resume-import-btn').parentElement).toHaveClass(
      'min-w-0',
      'flex-wrap',
    );
    const status = screen.getByTestId('resume-import-status');
    expect(status).toHaveAttribute('role', 'status');
    expect(status).toHaveAttribute('aria-live', 'polite');

    await fireEvent.click(screen.getByTestId('resume-import-btn'));
    await fireEvent.click(screen.getByTestId('dismiss-import-btn'));
    expect(onResumeImport).toHaveBeenCalledOnce();
    expect(onDismissImport).not.toHaveBeenCalled();
    const confirmation = get(showConfirmDialog);
    expect(confirmation?.title).toBe('Delete interrupted-import recovery?');
    expect(confirmation?.danger).toBe(true);
    await confirmation?.onConfirm();
    expect(onDismissImport).toHaveBeenCalledOnce();
    expect(screen.getByTestId('dismiss-import-btn')).toHaveTextContent('Delete recovery record');
  });

  it('blocks duplicate recovery actions while one durable command is in flight', () => {
    render(WorkstationRecoveryNotices, {
      quarantineNotice: null,
      interruptedImport: {
        id: 'import-job-1',
        totalFiles: 2,
        completedCount: 1,
        createdAt: '2026-08-28T10:00:00Z',
      },
      importRecoveryBusy: true,
      importRecoveryAuthority: 'known',
      workspaceOperationBusy: false,
      onAcknowledgeQuarantine: vi.fn(),
      onDismissQuarantine: vi.fn(),
      onResumeImport: vi.fn(),
      onDismissImport: vi.fn(),
      onRetryRecoveryCheck: vi.fn(),
    });

    expect(screen.getByTestId('resume-import-banner')).toHaveAttribute('aria-busy', 'true');
    expect(screen.getByTestId('resume-import-btn')).toBeDisabled();
    expect(screen.getByTestId('dismiss-import-btn')).toBeDisabled();
    expect(screen.getByTestId('resume-import-btn')).toHaveAccessibleDescription(
      'Finishing the current import recovery action.',
    );
  });

  it('does not start recovery while another workspace operation is active', async () => {
    const onResumeImport = vi.fn();
    const onDismissImport = vi.fn();
    render(WorkstationRecoveryNotices, {
      quarantineNotice: null,
      interruptedImport: {
        id: 'import-job-1',
        totalFiles: 2,
        completedCount: 1,
        createdAt: '2026-08-28T10:00:00Z',
      },
      importRecoveryBusy: false,
      importRecoveryAuthority: 'known',
      workspaceOperationBusy: true,
      onAcknowledgeQuarantine: vi.fn(),
      onDismissQuarantine: vi.fn(),
      onResumeImport,
      onDismissImport,
      onRetryRecoveryCheck: vi.fn(),
    });

    expect(screen.getByTestId('resume-import-btn')).toBeDisabled();
    expect(screen.getByTestId('dismiss-import-btn')).toBeDisabled();
    await fireEvent.click(screen.getByTestId('resume-import-btn'));
    await fireEvent.click(screen.getByTestId('dismiss-import-btn'));
    expect(onResumeImport).not.toHaveBeenCalled();
    expect(onDismissImport).not.toHaveBeenCalled();
  });

  it('fails closed while journal authority is unknown or being checked', async () => {
    const onResumeImport = vi.fn();
    const onDismissImport = vi.fn();
    const onRetryRecoveryCheck = vi.fn();
    const interruptedImport = {
      id: 'possibly-stale-job',
      totalFiles: 2,
      completedCount: 1,
      createdAt: '2026-08-28T10:00:00Z',
    };
    const view = render(WorkstationRecoveryNotices, {
      quarantineNotice: null,
      interruptedImport,
      importRecoveryBusy: false,
      importRecoveryAuthority: 'unknown',
      workspaceOperationBusy: false,
      onAcknowledgeQuarantine: vi.fn(),
      onDismissQuarantine: vi.fn(),
      onResumeImport,
      onDismissImport,
      onRetryRecoveryCheck,
    });

    expect(screen.queryByTestId('resume-import-btn')).not.toBeInTheDocument();
    await fireEvent.click(screen.getByTestId('retry-recovery-check-btn'));
    expect(onRetryRecoveryCheck).toHaveBeenCalledOnce();

    await view.rerender({
      quarantineNotice: null,
      interruptedImport,
      importRecoveryBusy: false,
      importRecoveryAuthority: 'checking',
      workspaceOperationBusy: false,
      onAcknowledgeQuarantine: vi.fn(),
      onDismissQuarantine: vi.fn(),
      onResumeImport,
      onDismissImport,
      onRetryRecoveryCheck,
    });

    expect(screen.getByTestId('resume-import-btn')).toBeDisabled();
    expect(screen.getByTestId('dismiss-import-btn')).toBeDisabled();
    await fireEvent.click(screen.getByTestId('resume-import-btn'));
    await fireEvent.click(screen.getByTestId('dismiss-import-btn'));
    expect(onResumeImport).not.toHaveBeenCalled();
    expect(onDismissImport).not.toHaveBeenCalled();
  });

  it('does not recheck unknown journal authority while a live workspace operation owns it', async () => {
    const onRetryRecoveryCheck = vi.fn();
    render(WorkstationRecoveryNotices, {
      quarantineNotice: null,
      interruptedImport: null,
      importRecoveryBusy: false,
      importRecoveryAuthority: 'unknown',
      workspaceOperationBusy: true,
      onAcknowledgeQuarantine: vi.fn(),
      onDismissQuarantine: vi.fn(),
      onResumeImport: vi.fn(),
      onDismissImport: vi.fn(),
      onRetryRecoveryCheck,
    });

    const retry = screen.getByTestId('retry-recovery-check-btn');
    expect(retry).toBeDisabled();
    expect(retry).toHaveAccessibleDescription(
      'Finish the current workspace operation before recovering an import.',
    );
    await fireEvent.click(retry);
    expect(onRetryRecoveryCheck).not.toHaveBeenCalled();
  });
});
