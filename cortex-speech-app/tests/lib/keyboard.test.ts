import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { KeyboardManager } from '../../src/lib/keyboard';

describe('KeyboardManager editable guard', () => {
  let km: KeyboardManager;
  let textarea: HTMLTextAreaElement;

  beforeEach(() => {
    km = new KeyboardManager();
    textarea = document.createElement('textarea');
    document.body.appendChild(textarea);
  });

  afterEach(() => {
    km.destroy();
    textarea.remove();
  });

  function press(target: Element, key: string, opts: { ctrl?: boolean; shift?: boolean } = {}) {
    const ev = new KeyboardEvent('keydown', {
      key,
      ctrlKey: opts.ctrl ?? false,
      shiftKey: opts.shift ?? false,
      bubbles: true,
      cancelable: true,
    });
    target.dispatchEvent(ev);
    return ev;
  }

  it('suppresses Ctrl+Z inside a textarea so native undo is preserved', () => {
    const undo = vi.fn();
    km.register({ key: 'z', ctrl: true, description: 'Undo', action: undo, category: 'edit' });

    const ev = press(textarea, 'z', { ctrl: true });

    expect(undo).not.toHaveBeenCalled();
    expect(ev.defaultPrevented).toBe(false); // native text undo must NOT be blocked
  });

  it('suppresses Ctrl+Shift+Z and Ctrl+D inside a textarea', () => {
    const redo = vi.fn();
    const verify = vi.fn();
    km.register({ key: 'z', ctrl: true, shift: true, description: 'Redo', action: redo, category: 'edit' });
    km.register({ key: 'd', ctrl: true, description: 'Toggle verified', action: verify, category: 'edit' });

    press(textarea, 'z', { ctrl: true, shift: true });
    press(textarea, 'd', { ctrl: true });

    expect(redo).not.toHaveBeenCalled();
    expect(verify).not.toHaveBeenCalled();
  });

  it('fires Ctrl+Z when focus is NOT in an editable element', () => {
    const undo = vi.fn();
    km.register({ key: 'z', ctrl: true, description: 'Undo', action: undo, category: 'edit' });

    const ev = press(document.body, 'z', { ctrl: true });

    expect(undo).toHaveBeenCalledOnce();
    expect(ev.defaultPrevented).toBe(true);
  });

  it('allows an allowInEditable shortcut (command palette) inside a textarea', () => {
    const palette = vi.fn();
    km.register({
      key: 'k',
      ctrl: true,
      description: 'Command palette',
      action: palette,
      category: 'general',
      allowInEditable: true,
    });

    const ev = press(textarea, 'k', { ctrl: true });

    expect(palette).toHaveBeenCalledOnce();
    expect(ev.defaultPrevented).toBe(true);
  });

  it('still suppresses bare keys inside a textarea', () => {
    const del = vi.fn();
    km.register({ key: 'Delete', description: 'Delete segment', action: del, category: 'edit' });

    press(textarea, 'Delete');

    expect(del).not.toHaveBeenCalled();
  });

  it('fires a bare navigation key when not in an editable', () => {
    const next = vi.fn();
    km.register({ key: 'j', description: 'Next segment', action: next, category: 'nav' });

    press(document.body, 'j');

    expect(next).toHaveBeenCalledOnce();
  });
});
