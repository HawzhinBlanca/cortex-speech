import { writable } from 'svelte/store';
import {
  formatPublicErrorReference,
  publicErrorReference,
  type PublicSuggestedAction,
} from '../errorText';

export type NotificationType = 'success' | 'error' | 'info' | 'warning';

export interface Notification {
  id: string;
  type: NotificationType;
  message: string;
  detail?: string;
  duration?: number;
  action?: { label: string; handler: () => void };
  suggestedAction?: PublicSuggestedAction;
  retryable?: boolean;
}

type ErrorNotificationOptions = {
  /** An arbitrary failure. Only its validated code/operation ID reaches the notification. */
  cause?: unknown;
  /** Compatibility input; treated as untrusted and reduced to a public error reference. */
  detail?: string;
  /** Already-localized, deliberately public detail. Never use this for backend output. */
  publicDetail?: string;
  action?: { label: string; handler: () => void };
};

type WarningNotificationOptions = {
  detail?: string;
  publicDetail?: string;
  cause?: unknown;
};

function createNotificationStore() {
  const { subscribe, update } = writable<Notification[]>([]);
  let counter = 0;

  function add(
    type: NotificationType,
    message: string,
    opts?: {
      detail?: string;
      duration?: number;
      action?: { label: string; handler: () => void };
    },
  ) {
    const id = `notif-${++counter}`;
    const notif: Notification = { id, type, message, ...opts };
    update((n) => [...n, notif]);
    if ((opts?.duration ?? 4000) > 0) {
      setTimeout(() => dismiss(id), opts?.duration ?? 4000);
    }
    return id;
  }

  function dismiss(id: string) {
    update((n) => n.filter((item) => item.id !== id));
  }

  function clear() {
    update(() => []);
  }

  return {
    subscribe,
    success: (msg: string, opts?: { detail?: string }) => add('success', msg, opts),
    error: (msg: string, opts?: ErrorNotificationOptions) => {
      const cause = opts?.cause ?? opts?.detail;
      const reference = publicErrorReference(cause);
      const referenceText = formatPublicErrorReference(cause);
      return add('error', msg, {
        ...(opts?.publicDetail
          ? { detail: opts.publicDetail }
          : referenceText
            ? { detail: referenceText }
            : {}),
        ...(opts?.action ? { action: opts.action } : {}),
        ...(reference.suggestedAction ? { suggestedAction: reference.suggestedAction } : {}),
        ...(typeof reference.retryable === 'boolean' ? { retryable: reference.retryable } : {}),
        duration: 8000,
      });
    },
    info: (msg: string, opts?: { detail?: string }) => add('info', msg, opts),
    warning: (msg: string, opts?: WarningNotificationOptions) => {
      const referenceText = formatPublicErrorReference(opts?.cause ?? opts?.detail);
      return add('warning', msg, {
        ...(opts?.publicDetail
          ? { detail: opts.publicDetail }
          : opts?.cause || opts?.detail
            ? referenceText
              ? { detail: referenceText }
              : {}
            : {}),
        duration: 6000,
      });
    },
    dismiss,
    clear,
  };
}

export const notifications = createNotificationStore();
