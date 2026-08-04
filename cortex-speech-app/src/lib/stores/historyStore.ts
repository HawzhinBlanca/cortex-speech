import { invoke } from '@tauri-apps/api/core';
import { writable } from 'svelte/store';
import { isTauriRuntime } from '../runtime';

export interface HistoryState {
  canUndo: boolean;
  canRedo: boolean;
  undoDescription: string | null;
  redoDescription: string | null;
  processing: boolean;
}

function createHistoryStore() {
  const store = writable<HistoryState>({
    canUndo: false,
    canRedo: false,
    undoDescription: null,
    redoDescription: null,
    processing: false,
  });

  async function refresh() {
    if (!isTauriRuntime()) {
      store.update((s) => ({ ...s, canUndo: false, canRedo: false }));
      return;
    }

    try {
      const [canUndo, canRedo] = await Promise.all([
        invoke<boolean>('can_undo'),
        invoke<boolean>('can_redo'),
      ]);
      store.update((s) => ({ ...s, canUndo, canRedo }));
    } catch {
      store.update((s) => ({ ...s, canUndo: false, canRedo: false }));
    }
  }

  return {
    subscribe: store.subscribe,
    async undo(): Promise<string | null> {
      if (!isTauriRuntime()) {
        store.update((s) => ({ ...s, canUndo: false, canRedo: false, processing: false }));
        return null;
      }

      store.update((s) => ({ ...s, processing: true }));
      try {
        const description = await invoke<string | null>('undo');
        await refresh();
        return description;
      } finally {
        store.update((s) => ({ ...s, processing: false }));
      }
    },
    async redo(): Promise<string | null> {
      if (!isTauriRuntime()) {
        store.update((s) => ({ ...s, canUndo: false, canRedo: false, processing: false }));
        return null;
      }

      store.update((s) => ({ ...s, processing: true }));
      try {
        const description = await invoke<string | null>('redo');
        await refresh();
        return description;
      } finally {
        store.update((s) => ({ ...s, processing: false }));
      }
    },
    refresh,
  };
}

export const historyStore = createHistoryStore();
