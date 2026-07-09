import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { KeyboardManager, physicalKey } from '../../src/lib/keyboard';

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

// True-10 audit BLOCKER regression gate: while a review surface owns the keyboard (Review &
// Correct mode / the Review Inbox overlay), global shortcuts acted on the HIDDEN curate selection —
// Ctrl+Enter/Ctrl+D verified an invisible segment with no human decision, Ctrl+T machine-overwrote
// it, Delete opened a confirm dialog UNDER the inbox. The manager must suppress every shortcut not
// explicitly marked allowInReview whenever the review probe is true.
describe('KeyboardManager review-surface guard', () => {
  let km: KeyboardManager;

  beforeEach(() => {
    km = new KeyboardManager();
  });

  afterEach(() => {
    km.destroy();
  });

  function press(key: string, opts: { ctrl?: boolean; code?: string } = {}) {
    const ev = new KeyboardEvent('keydown', {
      key,
      code: opts.code ?? '',
      ctrlKey: opts.ctrl ?? false,
      bubbles: true,
      cancelable: true,
    });
    document.body.dispatchEvent(ev);
    return ev;
  }

  it('suppresses non-allowlisted shortcuts while the review probe is true', () => {
    const verify = vi.fn();
    const del = vi.fn();
    const transcribe = vi.fn();
    km.register({ key: 'Enter', ctrl: true, description: 'Toggle verification', action: verify, category: 'edit' });
    km.register({ key: 'Delete', description: 'Delete segment', action: del, category: 'edit' });
    km.register({ key: 't', ctrl: true, description: 'Transcribe selected', action: transcribe, category: 'file' });
    km.setReviewSurfaceProbe(() => true);

    press('Enter', { ctrl: true });
    press('Delete');
    press('t', { ctrl: true });

    expect(verify).not.toHaveBeenCalled();
    expect(del).not.toHaveBeenCalled();
    expect(transcribe).not.toHaveBeenCalled();
  });

  it('lets an allowInReview shortcut (command palette) through on review surfaces', () => {
    const palette = vi.fn();
    km.register({
      key: 'k',
      ctrl: true,
      description: 'Command palette',
      action: palette,
      category: 'general',
      allowInReview: true,
    });
    km.setReviewSurfaceProbe(() => true);

    press('k', { ctrl: true });

    expect(palette).toHaveBeenCalledOnce();
  });

  it('fires normally again when the probe returns false', () => {
    const verify = vi.fn();
    km.register({ key: 'Enter', ctrl: true, description: 'Toggle verification', action: verify, category: 'edit' });
    km.setReviewSurfaceProbe(() => false);

    press('Enter', { ctrl: true });

    expect(verify).toHaveBeenCalledOnce();
  });

  it('probes at dispatch time, not registration time', () => {
    const verify = vi.fn();
    let inReview = false;
    km.setReviewSurfaceProbe(() => inReview);
    km.register({ key: 'd', ctrl: true, description: 'Toggle verified', action: verify, category: 'edit' });

    inReview = true;
    press('d', { ctrl: true });
    expect(verify).not.toHaveBeenCalled();

    inReview = false;
    press('d', { ctrl: true });
    expect(verify).toHaveBeenCalledOnce();
  });
});

// Layout-independence regression gate (true-10 audit): with the owner's Central Kurdish layout
// active, e.key is Arabic script ('ت' on the physical T key), which silently killed every letter
// shortcut. Matching must prefer the physical e.code.
describe('KeyboardManager physical-key (Kurdish layout) matching', () => {
  let km: KeyboardManager;

  beforeEach(() => {
    km = new KeyboardManager();
  });

  afterEach(() => {
    km.destroy();
  });

  function press(key: string, code: string, opts: { ctrl?: boolean } = {}) {
    const ev = new KeyboardEvent('keydown', {
      key,
      code,
      ctrlKey: opts.ctrl ?? false,
      bubbles: true,
      cancelable: true,
    });
    document.body.dispatchEvent(ev);
    return ev;
  }

  it('matches a letter shortcut by physical key under an Arabic-script layout', () => {
    const next = vi.fn();
    km.register({ key: 'j', description: 'Next segment', action: next, category: 'nav' });

    // Kurdish layout: physical J emits an Arabic-script e.key but code stays 'KeyJ'.
    press('ت', 'KeyJ');

    expect(next).toHaveBeenCalledOnce();
  });

  it('still matches by e.key when code is unavailable (synthetic/legacy events)', () => {
    const next = vi.fn();
    km.register({ key: 'j', description: 'Next segment', action: next, category: 'nav' });

    press('j', '');

    expect(next).toHaveBeenCalledOnce();
  });

  it('physicalKey maps KeyX/DigitN and passes non-letter keys through', () => {
    const mk = (key: string, code: string) => new KeyboardEvent('keydown', { key, code });
    expect(physicalKey(mk('ا', 'KeyA'))).toBe('a');
    expect(physicalKey(mk('!', 'Digit1'))).toBe('1');
    expect(physicalKey(mk('Enter', 'Enter'))).toBe('Enter');
    expect(physicalKey(mk(' ', 'Space'))).toBe(' ');
    expect(physicalKey(mk('Backspace', 'Backspace'))).toBe('Backspace');
  });
});
