import { writable } from 'svelte/store';

export type NotificationType = 'success' | 'error' | 'info' | 'warning';

export interface Notification {
  id: string;
  type: NotificationType;
  message: string;
  detail?: string;
  duration?: number;
  progress?: number;
  action?: { label: string; handler: () => void };
}

function createNotificationStore() {
  const { subscribe, update } = writable<Notification[]>([]);
  let counter = 0;

  function add(type: NotificationType, message: string, opts?: {
    detail?: string;
    duration?: number;
    progress?: number;
    action?: { label: string; handler: () => void };
  }) {
    const id = `notif-${++counter}`;
    const notif: Notification = { id, type, message, ...opts };
    update(n => [...n, notif]);
    if (!opts?.progress && (opts?.duration ?? 4000) > 0) {
      setTimeout(() => dismiss(id), opts?.duration ?? 4000);
    }
    return id;
  }

  function dismiss(id: string) {
    update(n => n.filter(item => item.id !== id));
  }

  function updateProgress(id: string, progress: number) {
    update(n => n.map(item => item.id === id ? { ...item, progress } : item));
  }

  function clear() {
    update(() => []);
  }

  return {
    subscribe,
    success: (msg: string, opts?: { detail?: string }) => add('success', msg, opts),
    error: (msg: string, opts?: { detail?: string; action?: { label: string; handler: () => void } }) =>
      add('error', msg, { ...opts, duration: 8000 }),
    info: (msg: string, opts?: { detail?: string }) => add('info', msg, opts),
    warning: (msg: string, opts?: { detail?: string }) => add('warning', msg, { ...opts, duration: 6000 }),
    progress: (msg: string, progress: number) => add('info', msg, { progress, duration: 0 }),
    dismiss,
    updateProgress,
    clear,
  };
}

export const notifications = createNotificationStore();
