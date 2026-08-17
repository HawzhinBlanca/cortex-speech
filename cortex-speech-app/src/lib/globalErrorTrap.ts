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

/**
 * Is this window 'error' event a real uncaught script error, or just noise?
 *
 * Exported so the behaviour is testable on its own — the two exclusions below were each a real
 * incident, and both must keep working:
 *
 *  * a resource that fails to load (`<img>`/`<audio>`/`<link>`/`<script>` 404 or CSP-blocked) fires
 *    'error' at the ELEMENT in the capture phase. A CSP-blocked Google-Fonts <link> once blanked the
 *    entire UI this way. A genuine uncaught script error always targets the window.
 *  * "ResizeObserver loop completed with undelivered notifications" is browser bookkeeping, not a
 *    failure, and it fires on ordinary layout churn.
 */
export function isUncaughtScriptError(event: ErrorEvent): boolean {
  if (event.target instanceof Element) return false;
  const message = event.message || '';
  return !message.includes('ResizeObserver') && !message.includes('Resize observer');
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
  // Uncaught SYNCHRONOUS errors, same destination and same reason (2026-08-17). Each ErrorBoundary
  // instance used to listen for this event itself, so one uncaught error made EVERY boundary in the
  // tree render its fallback at once — ten red panels for a single failure, in a UI where nine of
  // them were still perfectly fine. One listener, one toast, no cascade; a genuine RENDER failure is
  // still caught and shown in place by the boundary around the subtree that actually threw.
  window.addEventListener(
    'error',
    (event) => {
      if (!isUncaughtScriptError(event)) return;
      notifyUnhandledRejection(event.error ?? event.message);
    },
    true,
  );
}
