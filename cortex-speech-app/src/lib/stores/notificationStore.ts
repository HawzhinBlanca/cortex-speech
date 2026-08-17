import { writable } from 'svelte/store';

export type NotificationType = 'success' | 'error' | 'info' | 'warning';

export interface Notification {
  id: string;
  type: NotificationType;
  message: string;
  detail?: string;
  duration?: number;
  action?: { label: string; handler: () => void };
}

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
    error: (
      msg: string,
      opts?: { detail?: string; action?: { label: string; handler: () => void } },
    ) => add('error', msg, { ...opts, duration: 8000 }),
    info: (msg: string, opts?: { detail?: string }) => add('info', msg, opts),
    warning: (msg: string, opts?: { detail?: string }) =>
      add('warning', msg, { ...opts, duration: 6000 }),
    dismiss,
    clear,
  };
}

export const notifications = createNotificationStore();
