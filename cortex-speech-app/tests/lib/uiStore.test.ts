import { describe, it, expect, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import { showKeyboardHelp, showConfirmDialog, isProcessing, statusMessage } from '../../src/lib/stores/uiStore';

describe('uiStore', () => {
  beforeEach(() => {
    showKeyboardHelp.set(false);
    showConfirmDialog.set(null);
    isProcessing.set(false);
    statusMessage.set('Ready');
  });

  it('has defaults', () => {
    expect(get(showKeyboardHelp)).toBe(false);
    expect(get(showConfirmDialog)).toBeNull();
    expect(get(isProcessing)).toBe(false);
    expect(get(statusMessage)).toBe('Ready');
  });

  it('shows keyboard help', () => {
    showKeyboardHelp.set(true);
    expect(get(showKeyboardHelp)).toBe(true);
  });

  it('shows confirm dialog', () => {
    showConfirmDialog.set({ title: 'Confirm', message: 'Are you sure?', onConfirm: () => {} });
    expect(get(showConfirmDialog)?.message).toBe('Are you sure?');
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
