export interface Shortcut {
  key: string;
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
  description: string;
  action: () => void;
  category: string;
  /**
   * Allow this shortcut to fire while focus is in a text field / contentEditable (or during IME
   * composition). Default false: in an editable, ALL other shortcuts — bare keys AND modifier combos
   * like Ctrl+Z / Ctrl+Shift+Z / Ctrl+D — are suppressed so the field keeps native text editing
   * (undo/redo, etc.) and a reflexive Ctrl+Z can't hijack it or revert an unrelated app action. Mark
   * only genuinely editor-safe globals (command palette, focus-search) true.
   */
  allowInEditable?: boolean;
  /**
   * Allow this shortcut to fire while a review surface owns the keyboard (Review & Correct mode or
   * the Review Inbox overlay — see the shell's `setReviewSurfaceProbe`). Default false: the review
   * surfaces have their own keydown handlers and paired undo, while every global shortcut acts on
   * the HIDDEN curate selection — Ctrl+Enter/Ctrl+D verified an invisible segment with no human
   * decision (export-eligible gold nobody reviewed), Ctrl+T machine-overwrote it, and Delete opened
   * a confirm dialog UNDER the inbox overlay (true-10 audit blocker). Suppressing by default at the
   * manager gates future shortcuts too; mark only review-safe globals (command palette, undo/redo
   * whose handlers self-guard with a helpful notice) true.
   */
  allowInReview?: boolean;
}

/**
 * Layout-independent key identity: prefer the PHYSICAL key (e.code 'KeyA' → 'a', 'Digit1' → '1') so
 * letter/digit shortcuts keep working while the owner's Central Kurdish (Arabic-script) layout is
 * active — there e.key is 'ا'/'ب'/…, which silently killed every advertised review key (A/E/X/N/P,
 * j/k) until the OS layout was toggled back to Latin, once per edited clip. Non-letter keys
 * (Enter, arrows, '/', '?', ',', Space…) fall through to e.key unchanged.
 */
export function physicalKey(e: KeyboardEvent): string {
  const c = e.code || '';
  if (c.startsWith('Key') && c.length === 4) return c.slice(3).toLowerCase();
  if (c.startsWith('Digit') && c.length === 6) return c.slice(5);
  return e.key;
}

export class KeyboardManager {
  private shortcuts: Shortcut[] = [];
  private handler = (e: KeyboardEvent) => this.handleKeydown(e);
  /**
   * Supplied by the app shell: returns true while a review surface (Review & Correct mode or the
   * Review Inbox overlay) owns the keyboard. Checked at dispatch time so the suppression can never
   * go stale and applies to every registered and future shortcut by default.
   */
  private reviewSurfaceActive: () => boolean = () => false;

  constructor() {
    if (typeof window !== 'undefined') {
      window.addEventListener('keydown', this.handler);
    }
  }

  setReviewSurfaceProbe(probe: () => boolean) {
    this.reviewSurfaceActive = probe;
  }

  register(shortcut: Shortcut) {
    this.shortcuts.push(shortcut);
  }

  registerAll(shortcuts: Shortcut[]) {
    this.shortcuts.push(...shortcuts);
  }

  /** True while the user is typing into a field or composing text (IME). */
  private isFromEditable(e: KeyboardEvent): boolean {
    if (e.isComposing) return true;
    const t = e.target as HTMLElement | null;
    if (!t) return false;
    const tag = t.tagName;
    return (
      tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || t.isContentEditable === true
    );
  }

  private handleKeydown(e: KeyboardEvent) {
    const mod = e.metaKey || e.ctrlKey;
    const inEditable = this.isFromEditable(e);
    const inReview = this.reviewSurfaceActive();

    for (const s of this.shortcuts) {
      // While typing in a field or composing Kurdish/Arabic text, suppress EVERY shortcut that is not
      // explicitly editor-safe — bare keys (Delete, j/k, '/', Space…) AND modifier combos (Ctrl+Z,
      // Ctrl+Shift+Z, Ctrl+D…). This keeps native text editing intact: a reflexive Ctrl+Z performs the
      // field's own undo instead of being hijacked into a backend `undo` that reverts an unrelated
      // action and wipes the in-progress edit. Only allow-listed globals (command palette, focus
      // search) fire while editing.
      if (inEditable && !s.allowInEditable) continue;
      // While a review surface owns the keyboard, suppress every non-review-safe global: they act
      // on the HIDDEN curate selection (silent verify/transcribe/delete of an invisible segment —
      // gold corruption; see Shortcut.allowInReview).
      if (inReview && !s.allowInReview) continue;

      // Match on the physical key first (layout-independent — a Kurdish layout maps letter keys to
      // Arabic-script e.key values), then on e.key for non-letter/digit keys.
      const keyMatch =
        physicalKey(e).toLowerCase() === s.key.toLowerCase() ||
        e.key.toLowerCase() === s.key.toLowerCase();
      const ctrlMatch = !!s.ctrl === mod;
      const shiftMatch = !!s.shift === e.shiftKey;
      const altMatch = !!s.alt === e.altKey;
      if (keyMatch && ctrlMatch && shiftMatch && altMatch) {
        e.preventDefault();
        s.action();
        return;
      }
    }
  }

  getAll(): Shortcut[] {
    return [...this.shortcuts];
  }


  destroy() {
    if (typeof window !== 'undefined') {
      window.removeEventListener('keydown', this.handler);
    }
  }

  formatShortcut(s: Shortcut): string {
    const parts: string[] = [];
    if (s.ctrl) parts.push(modKeyLabel());
    if (s.shift) parts.push(isMacPlatform() ? '⇧' : 'Shift');
    if (s.alt) parts.push(isMacPlatform() ? '⌥' : 'Alt');
    parts.push(s.key.toUpperCase());
    return parts.join('+');
  }
}

/// True-10 audit: this app runs on Windows — showing Mac ⌘/⇧/⌥ glyphs everywhere was wrong.
/// Platform-aware labels, shared by the palette, help modal, and the hardcoded kbd hints.
export function isMacPlatform(): boolean {
  return typeof navigator !== 'undefined' && navigator.platform.includes('Mac');
}

export function modKeyLabel(): string {
  return isMacPlatform() ? '⌘' : 'Ctrl';
}

export let globalKeyboardManager: KeyboardManager | null = null;

export function initKeyboardManager() {
  globalKeyboardManager = new KeyboardManager();
  return globalKeyboardManager;
}
