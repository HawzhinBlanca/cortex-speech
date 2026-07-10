import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/svelte';
import { invoke } from '@tauri-apps/api/core';
import EngineStatusPill from '../../src/lib/EngineStatusPill.svelte';
import { locale } from '../../src/lib/i18n';

const invokeMock = vi.mocked(invoke);

describe('EngineStatusPill', () => {
  beforeEach(() => {
    locale.set('en');
    invokeMock.mockReset();
  });
  afterEach(() => {
    locale.set('ckb');
    vi.restoreAllMocks();
  });

  it('shows READY when the champion server answers, and no Start button', async () => {
    invokeMock.mockResolvedValue({ ready: true, port: 8799 });
    render(EngineStatusPill);
    await waitFor(() => expect(screen.getByText('Champion engine ready')).toBeInTheDocument());
    expect(screen.queryByTestId('engine-start-btn')).not.toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith('get_champion_engine_status');
  });

  it('shows OFFLINE + a Start button when the server is down', async () => {
    invokeMock.mockResolvedValue({ ready: false, port: 8799 });
    render(EngineStatusPill);
    await waitFor(() => expect(screen.getByText('Champion engine offline')).toBeInTheDocument());
    expect(screen.getByTestId('engine-start-btn')).toBeInTheDocument();
  });

  it('treats a failed status probe as offline (never throws to the UI)', async () => {
    invokeMock.mockRejectedValue(new Error('no wsl'));
    render(EngineStatusPill);
    await waitFor(() => expect(screen.getByText('Champion engine offline')).toBeInTheDocument());
  });

  it('Start invokes start_champion_engine and switches to the starting state', async () => {
    // First status probe: offline. Start resolves. A subsequent offline probe must NOT drop us out of
    // "starting" (the ~30 GB warm-up is still in progress).
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'start_champion_engine') return Promise.resolve(undefined);
      return Promise.resolve({ ready: false, port: 8799 });
    });
    render(EngineStatusPill);
    await waitFor(() => expect(screen.getByTestId('engine-start-btn')).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId('engine-start-btn'));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('start_champion_engine'));
    await waitFor(() => expect(screen.getByText('Starting engine…')).toBeInTheDocument());
    expect(screen.queryByTestId('engine-start-btn')).not.toBeInTheDocument();
  });
});
