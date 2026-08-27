import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import WorkstationRecoveryNotices from './WorkstationRecoveryNotices.svelte';
import { locale } from './i18n';

describe('WorkstationRecoveryNotices', () => {
  beforeEach(() => locale.set('en'));
  afterEach(() => {
    cleanup();
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
      onAcknowledgeQuarantine,
      onDismissQuarantine,
      onResumeImport: vi.fn(),
      onDismissImport: vi.fn(),
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
      onAcknowledgeQuarantine: vi.fn(),
      onDismissQuarantine: vi.fn(),
      onResumeImport,
      onDismissImport,
    });

    const banner = screen.getByTestId('resume-import-banner');
    expect(banner).toHaveTextContent('7/19 files done');
    expect(banner).not.toHaveTextContent('C:');

    await fireEvent.click(screen.getByTestId('resume-import-btn'));
    await fireEvent.click(screen.getByTestId('dismiss-import-btn'));
    expect(onResumeImport).toHaveBeenCalledOnce();
    expect(onDismissImport).toHaveBeenCalledOnce();
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
      onAcknowledgeQuarantine: vi.fn(),
      onDismissQuarantine: vi.fn(),
      onResumeImport: vi.fn(),
      onDismissImport: vi.fn(),
    });

    expect(screen.getByTestId('resume-import-banner')).toHaveAttribute('aria-busy', 'true');
    expect(screen.getByTestId('resume-import-btn')).toBeDisabled();
    expect(screen.getByTestId('dismiss-import-btn')).toBeDisabled();
    expect(screen.getByTestId('resume-import-btn')).toHaveAccessibleDescription(
      'Finishing the current import recovery action.',
    );
  });
});
