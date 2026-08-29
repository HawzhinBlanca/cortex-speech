/**
 * The single browser reload boundary used after replacing the live database.
 * Keeping it behind a module boundary lets tests prove the hand-off without asking jsdom to
 * perform navigation, which it intentionally does not implement.
 */
export function reloadApp(): void {
  window.location.reload();
}
