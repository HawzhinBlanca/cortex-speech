import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import WslConsolePanel from '../../src/lib/WslConsolePanel.svelte';
import { setLocale } from '../../src/lib/i18n';
import { notifications } from '../../src/lib/stores/notificationStore';
import { showWslConsole } from '../../src/lib/stores/uiStore';

const { subscribeDesktopEventMock } = vi.hoisted(() => ({
  subscribeDesktopEventMock: vi.fn(),
}));

vi.mock('../../src/lib/events', () => ({
  subscribeDesktopEvent: subscribeDesktopEventMock,
}));

type DesktopHandler = (event: { payload: unknown }) => void;

const invokeMock = vi.mocked(invoke);
const handlers = new Map<string, DesktopHandler>();
const unlisteners = new Map<string, ReturnType<typeof vi.fn>>();
const writeTextMock = vi.fn(async (_text: string): Promise<void> => {});

function emitDesktopEvent(event: string, payload: unknown): void {
  const handler = handlers.get(event);
  if (!handler) throw new Error(`No test handler registered for ${event}`);
  handler({ payload });
}

beforeEach(async () => {
  invokeMock.mockReset();
  handlers.clear();
  unlisteners.clear();
  subscribeDesktopEventMock.mockReset();
  subscribeDesktopEventMock.mockImplementation((event: string, handler: DesktopHandler) => {
    handlers.set(event, handler);
    const unlisten = vi.fn();
    unlisteners.set(event, unlisten);
    return Promise.resolve(unlisten);
  });
  writeTextMock.mockClear();
  Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    value: { writeText: writeTextMock },
  });
  notifications.clear();
  showWslConsole.set(true);
  await setLocale('en');
});

afterEach(() => {
  cleanup();
  notifications.clear();
  showWslConsole.set(false);
  vi.restoreAllMocks();
});

describe('WslConsolePanel owner workstation flow', () => {
  it('starts idle, subscribes to both desktop streams, and closes through every idle affordance', async () => {
    const view = render(WslConsolePanel);

    expect(screen.getByRole('dialog')).toHaveAccessibleName(
      'Meta OmniASR 7B v2 local transcription (WSL)',
    );
    expect(screen.getByText('Idle')).toBeInTheDocument();
    expect(screen.getByText(/No console logs to display/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Clear Logs' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Copy Logs' })).toBeDisabled();
    expect(subscribeDesktopEventMock.mock.calls.map(([event]) => event)).toEqual([
      'wsl-log',
      'wsl-status',
    ]);

    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    expect(get(showWslConsole)).toBe(false);

    showWslConsole.set(true);
    await fireEvent.click(screen.getByRole('dialog'));
    expect(get(showWslConsole)).toBe(false);

    showWslConsole.set(true);
    await fireEvent.click(screen.getAllByRole('button', { name: 'Close' })[0]);
    expect(get(showWslConsole)).toBe(false);

    view.unmount();
    await waitFor(() => {
      expect(unlisteners.get('wsl-log')).toHaveBeenCalledOnce();
      expect(unlisteners.get('wsl-status')).toHaveBeenCalledOnce();
    });
  });

  it('runs with exact limits, keeps the modal open, renders streamed logs, and completes with counts', async () => {
    invokeMock.mockResolvedValue({ status: 'started' });
    render(WslConsolePanel);

    await fireEvent.input(screen.getByLabelText(/Limit Files/), { target: { value: '2' } });
    await fireEvent.input(screen.getByLabelText(/Limit Segments per file/), {
      target: { value: '7' },
    });
    await fireEvent.click(screen.getByRole('checkbox', { name: /Dry Run/ }));
    await fireEvent.click(screen.getByRole('checkbox', { name: /Test Mode/ }));
    await fireEvent.click(screen.getByRole('button', { name: 'Start local 7B batch ASR' }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith('run_wsl_refinement', {
        limitFiles: 2,
        limitSegments: 7,
        dryRun: true,
        testOne: true,
      }),
    );
    expect(screen.getByRole('status')).toHaveTextContent('Processing...');
    expect(screen.getByLabelText(/Limit Files/)).toBeDisabled();
    expect(screen.getAllByRole('button', { name: 'Close' })).toEqual([
      expect.objectContaining({ disabled: true }),
      expect.objectContaining({ disabled: true }),
    ]);
    expect(screen.getByRole('log')).toHaveTextContent('files at most 2');
    expect(screen.getByRole('log')).toHaveTextContent('segments at most 7');
    expect(screen.getByRole('log')).toHaveTextContent('dry run (no writes)');
    expect(screen.getByRole('log')).toHaveTextContent('test one (single segment)');

    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    expect(get(showWslConsole)).toBe(true);
    expect(get(notifications).at(-1)).toMatchObject({
      type: 'warning',
      message: 'Refinement is still running. Please cancel or wait for it to complete.',
    });

    emitDesktopEvent('wsl-log', '[ERROR] one clip failed');
    emitDesktopEvent('wsl-log', 'model loaded successfully');
    emitDesktopEvent('wsl-log', 'ordinary progress');
    emitDesktopEvent('wsl-log', 'Complete!');
    await waitFor(() =>
      expect(screen.getByRole('log')).toHaveTextContent('[ERROR] one clip failed'),
    );
    expect(screen.getByRole('log')).toHaveTextContent('model loaded successfully');
    expect(screen.getByRole('log')).toHaveTextContent('ordinary progress');

    emitDesktopEvent('wsl-status', {
      status: 'completed',
      transcribed: 6,
      failed: 1,
      exit_code: 0,
    });
    expect(await screen.findByText('6 completed; 1 failed')).toBeInTheDocument();
    expect(screen.getByLabelText(/Limit Files/)).not.toBeDisabled();
    expect(screen.getByRole('button', { name: 'Start local 7B batch ASR' })).toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: 'Copy Logs' }));
    expect(writeTextMock).toHaveBeenCalledOnce();
    expect(writeTextMock.mock.calls[0][0]).toContain('[ERROR] one clip failed');
    expect(get(notifications).at(-1)).toMatchObject({
      type: 'success',
      message: 'Console logs copied to clipboard',
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Clear Logs' }));
    expect(screen.getByText(/No console logs to display/)).toBeInTheDocument();
  });

  it('uses the all-pending defaults and exposes completed, failed, and cancelled terminal states', async () => {
    invokeMock.mockResolvedValue({ status: 'started' });
    render(WslConsolePanel);

    await fireEvent.click(screen.getByRole('button', { name: 'Start local 7B batch ASR' }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith('run_wsl_refinement', {
        limitFiles: null,
        limitSegments: null,
        dryRun: false,
        testOne: false,
      }),
    );
    expect(screen.getByRole('log')).toHaveTextContent('Options: all pending segments');

    emitDesktopEvent('wsl-status', { status: 'completed', exit_code: 0 });
    expect(await screen.findByText('Completed Successfully')).toBeInTheDocument();
    emitDesktopEvent('wsl-status', { status: 'failed', exit_code: 1 });
    expect(await screen.findByText('Process Failed')).toBeInTheDocument();
    emitDesktopEvent('wsl-status', { status: 'cancelled', exit_code: 130 });
    expect(await screen.findByText('Cancelled')).toBeInTheDocument();
  });

  it('surfaces start failures as technical console evidence and a scrubbed notification', async () => {
    invokeMock.mockRejectedValueOnce(
      Object.assign(new Error('private WSL path C:\\owner\\models'), {
        code: 'WSL_START_FAILED',
      }),
    );
    render(WslConsolePanel);

    await fireEvent.click(screen.getByRole('button', { name: 'Start local 7B batch ASR' }));

    expect(await screen.findByText('Process Failed')).toBeInTheDocument();
    expect(screen.getByRole('log')).toHaveTextContent(
      '[SYSTEM ERROR] Failed to start refinement: private WSL path C:\\owner\\models',
    );
    expect(get(notifications).at(-1)).toMatchObject({
      type: 'error',
      message: 'Failed to start WSL refinement process',
    });
    expect(screen.getByRole('button', { name: 'Start local 7B batch ASR' })).toBeInTheDocument();
  });

  it('requests cancellation and handles a failed cancellation without losing the running state', async () => {
    invokeMock.mockImplementation((command: string) => {
      if (command === 'run_wsl_refinement') return Promise.resolve({ status: 'started' });
      if (command === 'cancel_wsl_refinement') {
        return Promise.reject({
          schema: 1,
          code: 'CANCEL_FAILED',
          message: 'private backend reason',
          retryable: true,
        });
      }
      return Promise.reject(new Error(`Unexpected command: ${command}`));
    });
    render(WslConsolePanel);

    await fireEvent.click(screen.getByRole('button', { name: 'Start local 7B batch ASR' }));
    await fireEvent.click(await screen.findByRole('button', { name: 'Cancel/Stop' }));

    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('cancel_wsl_refinement'));
    expect(screen.getByRole('log')).toHaveTextContent('[SYSTEM ERROR] Cancellation failed:');
    expect(screen.getByRole('log')).toHaveTextContent('CANCEL_FAILED');
    expect(screen.getByRole('log')).toHaveTextContent('private backend reason');
    expect(get(notifications).at(-1)).toMatchObject({
      type: 'error',
      message: 'Failed to cancel WSL refinement',
      retryable: true,
    });
    expect(screen.getByRole('button', { name: 'Cancel/Stop' })).toBeInTheDocument();

    invokeMock.mockResolvedValueOnce(null);
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel/Stop' }));
    expect(await screen.findByText('Cancelled')).toBeInTheDocument();
  });

  it('tolerates listener setup rejection during synchronous teardown', async () => {
    subscribeDesktopEventMock.mockImplementation(() => Promise.reject(new Error('window gone')));
    const view = render(WslConsolePanel);

    expect(screen.getByText('Idle')).toBeInTheDocument();
    expect(() => view.unmount()).not.toThrow();
    await Promise.resolve();
    await Promise.resolve();
  });
});
