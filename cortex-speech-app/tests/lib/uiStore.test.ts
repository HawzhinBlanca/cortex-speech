import { describe, it, expect, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import {
  viewMode,
  leftPanel,
  rightPanel,
  showKeyboardHelp,
  showConfirmDialog,
  isProcessing,
  statusMessage,
  toggleLeftPanel,
  toggleRightPanel,
} from '../../src/lib/stores/uiStore';

describe('uiStore', () => {
  beforeEach(() => {
    viewMode.set('transcribe');
    leftPanel.set('segments');
    rightPanel.set('none');
    showKeyboardHelp.set(false);
    showConfirmDialog.set(null);
    isProcessing.set(false);
    statusMessage.set('Ready');
  });

  it('has defaults', () => {
    expect(get(viewMode)).toBe('transcribe');
    expect(get(leftPanel)).toBe('segments');
    expect(get(rightPanel)).toBe('none');
    expect(get(showKeyboardHelp)).toBe(false);
    expect(get(showConfirmDialog)).toBeNull();
    expect(get(isProcessing)).toBe(false);
    expect(get(statusMessage)).toBe('Ready');
  });

  it('toggles left panel', () => {
    toggleLeftPanel();
    expect(get(leftPanel)).toBe('stats');
    toggleLeftPanel();
    expect(get(leftPanel)).toBe('segments');
  });

  it('toggles right panel', () => {
    toggleRightPanel();
    expect(get(rightPanel)).not.toBe('none');
    toggleRightPanel();
    expect(get(rightPanel)).toBe('none');
  });

  it('shows keyboard help', () => {
    showKeyboardHelp.set(true);
    expect(get(showKeyboardHelp)).toBe(true);
  });

  it('shows confirm dialog', () => {
    showConfirmDialog.set({ title: 'Confirm', message: 'Are you sure?', onConfirm: () => {} });
    expect(get(showConfirmDialog)?.message).toBe('Are you sure?');
  });

  it('sets view mode', () => {
    viewMode.set('curate');
    expect(get(viewMode)).toBe('curate');
    viewMode.set('align');
    expect(get(viewMode)).toBe('align');
  });

  it('sets processing state', () => {
    isProcessing.set(true);
    expect(get(isProcessing)).toBe(true);
    isProcessing.set(false);
    expect(get(isProcessing)).toBe(false);
  });

  it('sets status message', () => {
    statusMessage.set('Importing files...');
    expect(get(statusMessage)).toBe('Importing files...');
  });
});
