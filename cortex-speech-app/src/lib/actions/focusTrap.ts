/**
 * Svelte action that traps keyboard focus inside a node while it is mounted,
 * moves initial focus into it, and restores focus to the opener on teardown.
 * Use on a modal/dialog rendered behind `{#if open}` so mount = open, destroy = close.
 */
const FOCUSABLE =
  'a[href],button:not([disabled]),textarea:not([disabled]),input:not([disabled]),select:not([disabled]),[tabindex]:not([tabindex="-1"])';

export function focusTrap(node: HTMLElement) {
  const previouslyFocused = (document.activeElement as HTMLElement | null) ?? null;

  const items = (): HTMLElement[] =>
    Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
      (el) => el.offsetWidth > 0 || el.offsetHeight > 0 || el === document.activeElement,
    );

  function onKeydown(e: KeyboardEvent) {
    if (e.key !== 'Tab') return;
    const list = items();
    if (list.length === 0) {
      e.preventDefault();
      node.focus();
      return;
    }
    const first = list[0];
    const last = list[list.length - 1];
    const active = document.activeElement as HTMLElement;
    if (e.shiftKey) {
      if (active === first || !node.contains(active)) {
        e.preventDefault();
        last.focus();
      }
    } else if (active === last || !node.contains(active)) {
      e.preventDefault();
      first.focus();
    }
  }

  node.addEventListener('keydown', onKeydown);

  // Move focus inside: prefer an [autofocus] element, else the first focusable.
  requestAnimationFrame(() => {
    const auto = node.querySelector<HTMLElement>('[autofocus]');
    (auto ?? items()[0] ?? node).focus();
  });

  return {
    destroy() {
      node.removeEventListener('keydown', onKeydown);
      // Restore focus to whatever opened the dialog (if still in the document).
      if (previouslyFocused && document.contains(previouslyFocused)) {
        previouslyFocused.focus();
      }
    },
  };
}
