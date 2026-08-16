import { get } from 'svelte/store';
import { notifications } from './stores/notificationStore';
import { t } from './i18n';
import { formatUnknownError } from './errorText';

/**
 * Extract a human-readable string from an unhandled promise-rejection reason (an Error, a bare string,
 * or an arbitrary object) — used as the toast detail.
 */
export function describeRejection(reason: unknown): string {
  return formatUnknownError(reason);
}

/** Surface a fire-and-forget promise rejection as an error toast (never let it vanish). */
export function notifyUnhandledRejection(reason: unknown): void {
  notifications.error(get(t)('notifications.unexpectedError'), {
    detail: describeRejection(reason),
  });
}

let installed = false;

/**
 * P2.2 (audit F3): install a global `unhandledrejection` trap. Before this, ErrorBoundary hooked only
 * synchronous window 'error' events, so a REJECTED un-awaited promise — e.g. an
 * `onclick={() => invoke(...)}` whose IPC fails, or a teardown write losing to a closed webview —
 * vanished into the console with no user-visible trace. Routes to the NOTIFICATION system (a toast),
 * NOT a panel-blanking ErrorBoundary, so a background failure never crashes the whole UI. Idempotent,
 * and the browser's own console logging is left intact (we do not preventDefault).
 */
export function installGlobalErrorTrap(): void {
  if (installed || typeof window === 'undefined') return;
  installed = true;
  window.addEventListener('unhandledrejection', (event) => {
    notifyUnhandledRejection(event.reason);
  });
}
