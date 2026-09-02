import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import HistoryPanel from '../../src/lib/HistoryPanel.svelte';
import { historyStore } from '../../src/lib/stores/historyStore';
import { locale } from '../../src/lib/i18n';
import { invoke } from '@tauri-apps/api/core';

describe('HistoryPanel', () => {
  beforeEach(() => {
    // This file asserts the English title copy; the app defaults to Sorani, so pin
    // English here and restore the default afterwards (other files assert ckb text).
    locale.set('en');
    window.__TAURI__ = {};
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_history_status_v1') {
        return Promise.resolve({ undoAction: 'updateSegment', redoAction: 'deleteSegments' });
      }
      return Promise.resolve(null);
    });
  });

  afterEach(() => {
    cleanup();
    locale.set('ckb');
    delete window.__TAURI__;
    delete window.__TAURI_INTERNALS__;
  });

  it('renders history stack title', async () => {
    render(HistoryPanel);
    await historyStore.refresh();
    expect(screen.getByText('History Stack')).toBeInTheDocument();
  });

  it('does not render hotkey overlays by default', async () => {
    render(HistoryPanel, { props: { showHotkeyOverlay: false } });
    await historyStore.refresh();
    await waitFor(() => {
      expect(screen.queryByText('^Z')).not.toBeInTheDocument();
      expect(screen.queryByText('^+Z')).not.toBeInTheDocument();
    });
  });

  it('renders hotkey overlays when showHotkeyOverlay is true', async () => {
    render(HistoryPanel, { props: { showHotkeyOverlay: true } });
    await historyStore.refresh();
    await waitFor(() => {
      expect(screen.getByText('^Z')).toBeInTheDocument();
      expect(screen.getByText('^+Z')).toBeInTheDocument();
    });
  });

  it('keeps history disabled in browser mode without invoking Tauri', async () => {
    delete window.__TAURI__;
    delete window.__TAURI_INTERNALS__;

    render(HistoryPanel, { props: { showHotkeyOverlay: true } });
    await historyStore.refresh();
    const undoResult = await historyStore.undo();
    const redoResult = await historyStore.redo();

    expect(undoResult).toEqual({ action: null, status: { undoAction: null, redoAction: null } });
    expect(redoResult).toEqual({ action: null, status: { undoAction: null, redoAction: null } });
    expect(vi.mocked(invoke)).not.toHaveBeenCalled();
    await waitFor(() => {
      expect(screen.queryByText('^Z')).not.toBeInTheDocument();
      expect(screen.queryByText('^+Z')).not.toBeInTheDocument();
    });
  });
});
