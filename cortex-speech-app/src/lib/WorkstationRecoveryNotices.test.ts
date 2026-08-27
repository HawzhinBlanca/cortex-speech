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
});
