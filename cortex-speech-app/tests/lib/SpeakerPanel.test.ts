import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import SpeakerPanel from '../../src/lib/SpeakerPanel.svelte';
import { setLocale } from '../../src/lib/i18n';
import { historyStore } from '../../src/lib/stores/historyStore';
import { notifications } from '../../src/lib/stores/notificationStore';
import { segments } from '../../src/lib/stores/segmentStore';
import { showSpeakerPanel } from '../../src/lib/stores/uiStore';

const invokeMock = vi.mocked(invoke);

const initialInventory = [
  { speakerId: 'SPEAKER_A', segmentCount: 2, totalDurationSeconds: 90 },
  { speakerId: 'SPEAKER_B', segmentCount: 3, totalDurationSeconds: 120 },
  { speakerId: null, segmentCount: 1, totalDurationSeconds: 30 },
];

beforeEach(async () => {
  invokeMock.mockReset();
  notifications.clear();
  showSpeakerPanel.set(true);
  await setLocale('en');
  vi.spyOn(historyStore, 'refresh').mockResolvedValue();
  vi.spyOn(segments, 'load').mockResolvedValue(true);
  vi.spyOn(window, 'confirm').mockReturnValue(true);
});

afterEach(() => {
  cleanup();
  notifications.clear();
  showSpeakerPanel.set(false);
  vi.restoreAllMocks();
});

describe('SpeakerPanel atomic inventory workflow', () => {
  it('loads the complete inventory, preserves SQL NULL as Unassigned, and closes from every affordance', async () => {
    let resolveInventory: ((value: typeof initialInventory) => void) | undefined;
    invokeMock.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveInventory = resolve;
        }),
    );
    render(SpeakerPanel);

    expect(screen.getByRole('status', { name: 'Loading...' })).toBeInTheDocument();
    resolveInventory?.(initialInventory);
    expect(await screen.findByText('SPEAKER_A')).toBeInTheDocument();
    expect(screen.getByText('2 segments · 1.5 min')).toBeInTheDocument();
    expect(screen.getByText('Unassigned')).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith('get_speaker_inventory_v1');

    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    expect(get(showSpeakerPanel)).toBe(false);
    showSpeakerPanel.set(true);
    await fireEvent.click(screen.getByRole('dialog'));
    expect(get(showSpeakerPanel)).toBe(false);
    showSpeakerPanel.set(true);
    await fireEvent.click(screen.getAllByRole('button', { name: 'Close' })[0]);
    expect(get(showSpeakerPanel)).toBe(false);
  });

  it('renders an honest empty state for null or empty inventory and surfaces load failure', async () => {
    invokeMock.mockResolvedValueOnce(null);
    const emptyView = render(SpeakerPanel);
    expect(await screen.findByText('No speakers identified yet.')).toBeInTheDocument();
    emptyView.unmount();

    invokeMock.mockRejectedValueOnce({
      schema: 1,
      code: 'SPEAKER_INVENTORY_FAILED',
      message: 'private database path',
      retryable: true,
    });
    render(SpeakerPanel);
    await waitFor(() =>
      expect(get(notifications).at(-1)).toMatchObject({
        type: 'error',
        message: 'Failed to load speakers',
        retryable: true,
      }),
    );
    expect(screen.getByText('No speakers identified yet.')).toBeInTheDocument();
  });

  it('rejects blank and semantic no-op edits without minting history', async () => {
    invokeMock.mockResolvedValue(initialInventory);
    render(SpeakerPanel);
    await screen.findByText('SPEAKER_A');

    await fireEvent.click(screen.getAllByRole('button', { name: 'Rename' })[0]);
    const input = screen.getByRole('textbox', { name: '' });
    expect(input).toHaveValue('SPEAKER_A');
    await fireEvent.input(input, { target: { value: '   ' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(screen.getByRole('textbox')).toHaveValue('   ');
    expect(invokeMock.mock.calls.filter(([command]) => command === 'rename_speaker_v1')).toEqual(
      [],
    );

    await fireEvent.input(screen.getByRole('textbox'), { target: { value: ' SPEAKER_A ' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(historyStore.refresh).not.toHaveBeenCalled();

    await fireEvent.click(screen.getAllByRole('button', { name: 'Rename' })[0]);
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
  });

  it('requires explicit merge confirmation and submits source/target inventory counts atomically', async () => {
    let inventoryReads = 0;
    invokeMock.mockImplementation((command: string) => {
      if (command === 'get_speaker_inventory_v1') {
        inventoryReads += 1;
        return Promise.resolve(
          inventoryReads === 1
            ? initialInventory
            : [
                { speakerId: 'SPEAKER_B', segmentCount: 5, totalDurationSeconds: 210 },
                { speakerId: null, segmentCount: 1, totalDurationSeconds: 30 },
              ],
        );
      }
      if (command === 'rename_speaker_v1') {
        return Promise.resolve({
          sourceSpeakerId: 'SPEAKER_A',
          targetSpeakerId: 'SPEAKER_B',
          renamedCount: 2,
          targetCount: 5,
          merged: true,
        });
      }
      return Promise.reject(new Error(`Unexpected command: ${command}`));
    });
    const confirmMock = vi.mocked(window.confirm);
    confirmMock.mockReturnValueOnce(false).mockReturnValueOnce(true);
    render(SpeakerPanel);
    await screen.findByText('SPEAKER_A');

    await fireEvent.click(screen.getAllByRole('button', { name: 'Rename' })[0]);
    await fireEvent.input(screen.getByRole('textbox'), { target: { value: ' SPEAKER_B ' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(confirmMock).toHaveBeenCalledWith(expect.stringContaining("'SPEAKER_B' already exists"));
    expect(invokeMock.mock.calls.filter(([command]) => command === 'rename_speaker_v1')).toEqual(
      [],
    );
    expect(screen.getByRole('textbox')).toHaveValue(' SPEAKER_B ');

    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith('rename_speaker_v1', {
        request: {
          sourceSpeakerId: 'SPEAKER_A',
          targetSpeakerId: 'SPEAKER_B',
          expectedSourceCount: 2,
          expectedTargetCount: 3,
        },
      }),
    );
    expect(get(notifications).at(-1)).toMatchObject({
      type: 'success',
      message: 'Updated 2 segments',
    });
    await waitFor(() => {
      expect(historyStore.refresh).toHaveBeenCalledOnce();
      expect(segments.load).toHaveBeenCalledOnce();
      expect(inventoryReads).toBe(2);
    });
    expect(await screen.findByText('5 segments · 3.5 min')).toBeInTheDocument();
  });

  it('retains a failed proposal and refreshes authority only for a stale inventory conflict', async () => {
    let inventoryReads = 0;
    invokeMock.mockImplementation((command: string) => {
      if (command === 'get_speaker_inventory_v1') {
        inventoryReads += 1;
        return Promise.resolve(
          inventoryReads === 1
            ? initialInventory
            : [
                { speakerId: 'SPEAKER_A', segmentCount: 4, totalDurationSeconds: 150 },
                { speakerId: 'SPEAKER_B', segmentCount: 3, totalDurationSeconds: 120 },
              ],
        );
      }
      if (command === 'rename_speaker_v1') {
        return Promise.reject({
          schema: 1,
          code: 'STALE_SPEAKER_INVENTORY',
          message: 'private conflict detail',
          retryable: false,
          suggestedAction: 'reloadClip',
        });
      }
      return Promise.reject(new Error(`Unexpected command: ${command}`));
    });
    render(SpeakerPanel);
    await screen.findByText('SPEAKER_A');

    await fireEvent.click(screen.getAllByRole('button', { name: 'Rename' })[0]);
    await fireEvent.input(screen.getByRole('textbox'), { target: { value: 'SPEAKER_C' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(inventoryReads).toBe(2));
    expect(screen.getByRole('textbox')).toHaveValue('SPEAKER_C');
    expect(screen.getByText('4 segments · 2.5 min')).toBeInTheDocument();
    expect(get(notifications).at(-1)).toMatchObject({
      type: 'error',
      message: 'Rename failed',
      suggestedAction: 'reloadClip',
    });
    expect(historyStore.refresh).not.toHaveBeenCalled();
    expect(segments.load).not.toHaveBeenCalled();
  });

  it('does not refresh inventory for a non-conflict rename failure', async () => {
    invokeMock.mockImplementation((command: string) => {
      if (command === 'get_speaker_inventory_v1') return Promise.resolve(initialInventory);
      if (command === 'rename_speaker_v1') return Promise.reject(new Error('write failed'));
      return Promise.reject(new Error(`Unexpected command: ${command}`));
    });
    render(SpeakerPanel);
    await screen.findByText('SPEAKER_A');

    await fireEvent.click(screen.getAllByRole('button', { name: 'Rename' })[0]);
    await fireEvent.input(screen.getByRole('textbox'), { target: { value: 'SPEAKER_C' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() =>
      expect(get(notifications).at(-1)).toMatchObject({ type: 'error', message: 'Rename failed' }),
    );
    expect(
      invokeMock.mock.calls.filter(([command]) => command === 'get_speaker_inventory_v1'),
    ).toHaveLength(1);
    expect(screen.getByRole('textbox')).toHaveValue('SPEAKER_C');
  });
});
