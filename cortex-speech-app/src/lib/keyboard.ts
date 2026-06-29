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
}

export class KeyboardManager {
  private shortcuts: Shortcut[] = [];
  private handler = (e: KeyboardEvent) => this.handleKeydown(e);

  constructor() {
    if (typeof window !== 'undefined') {
      window.addEventListener('keydown', this.handler);
    }
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

    for (const s of this.shortcuts) {
      // While typing in a field or composing Kurdish/Arabic text, suppress EVERY shortcut that is not
      // explicitly editor-safe — bare keys (Delete, j/k, '/', Space…) AND modifier combos (Ctrl+Z,
      // Ctrl+Shift+Z, Ctrl+D…). This keeps native text editing intact: a reflexive Ctrl+Z performs the
      // field's own undo instead of being hijacked into a backend `undo` that reverts an unrelated
      // action and wipes the in-progress edit. Only allow-listed globals (command palette, focus
      // search) fire while editing.
      if (inEditable && !s.allowInEditable) continue;

      const keyMatch = e.key.toLowerCase() === s.key.toLowerCase();
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

  getByCategory(category: string): Shortcut[] {
    return this.shortcuts.filter((s) => s.category === category);
  }

  destroy() {
    if (typeof window !== 'undefined') {
      window.removeEventListener('keydown', this.handler);
    }
  }

  formatShortcut(s: Shortcut): string {
    const parts: string[] = [];
    if (s.ctrl) parts.push('⌘');
    if (s.shift) parts.push('⇧');
    if (s.alt) parts.push('⌥');
    parts.push(s.key.toUpperCase());
    return parts.join('+');
  }
}

export let globalKeyboardManager: KeyboardManager | null = null;

export function initKeyboardManager() {
  globalKeyboardManager = new KeyboardManager();
  return globalKeyboardManager;
}
