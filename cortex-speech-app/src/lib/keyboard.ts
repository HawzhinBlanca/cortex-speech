export interface Shortcut {
  key: string;
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
  description: string;
  action: () => void;
  category: string;
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

    // Don't let bare shortcuts (Delete, j/k, '/', '?', Space…) fire while the user is
    // typing in a field or composing Kurdish/Arabic text — only modifier shortcuts
    // (Ctrl/Cmd + …) pass through. Prevents editing from silently mutating data.
    if (!mod && this.isFromEditable(e)) return;

    for (const s of this.shortcuts) {
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
