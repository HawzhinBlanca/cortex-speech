import { describe, it, expect, vi, afterEach } from 'vitest';
import { KeyboardManager, type Shortcut } from './keyboard';

/**
 * A blocking modal dialog (Settings, ConfirmDialog, Keyboard help, Speaker/Validation panels, command
 * palette — all set aria-modal="true") owns the keyboard. A global shortcut like Ctrl+D (toggle verify) or
 * Ctrl+Enter firing UNDERNEATH it would flip `verified` / delete the HIDDEN curate selection behind the
 * dialog — a silently-wrong human-gold label. The manager must suppress every global shortcut while any
 * aria-modal dialog is open.
 */
describe('KeyboardManager modal gate', () => {
  afterEach(() => {
    document.body.innerHTML = '';
  });

  const verifyChord = (action: () => void): Shortcut => ({
    key: 'd',
    ctrl: true,
    description: 'Toggle verify',
    category: 'test',
    action,
  });

  const pressCtrlD = () =>
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'd', code: 'KeyD', ctrlKey: true, bubbles: true }));

  it('fires a global shortcut when no modal is open', () => {
    const km = new KeyboardManager();
    const action = vi.fn();
    km.register(verifyChord(action));

    pressCtrlD();
    expect(action).toHaveBeenCalledTimes(1);

    km.destroy();
  });

  it('suppresses global shortcuts while an aria-modal dialog is open', () => {
    const km = new KeyboardManager();
    const action = vi.fn();
    km.register(verifyChord(action));

    // Open a blocking modal (every dialog sets aria-modal="true").
    const modal = document.createElement('div');
    modal.setAttribute('role', 'dialog');
    modal.setAttribute('aria-modal', 'true');
    document.body.appendChild(modal);

    pressCtrlD();
    expect(action).not.toHaveBeenCalled();

    // Closing the modal restores the shortcut.
    modal.remove();
    pressCtrlD();
    expect(action).toHaveBeenCalledTimes(1);

    km.destroy();
  });
});
