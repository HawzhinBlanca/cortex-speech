import { writable } from 'svelte/store';
import {
  getHistoryStatusV1,
  redo,
  undo,
  type HistoryActionV1,
  type HistoryMutationResultV1,
  type HistoryStatusV1,
} from '../commands';
import { isTauriRuntime } from '../runtime';

export interface HistoryState {
  canUndo: boolean;
  canRedo: boolean;
  undoAction: HistoryActionV1 | null;
  redoAction: HistoryActionV1 | null;
  processing: boolean;
}

const emptyStatus: HistoryStatusV1 = { undoAction: null, redoAction: null };

function stateFromStatus(status: HistoryStatusV1, processing: boolean): HistoryState {
  return {
    canUndo: status.undoAction !== null,
    canRedo: status.redoAction !== null,
    undoAction: status.undoAction,
    redoAction: status.redoAction,
    processing,
  };
}

function createHistoryStore() {
  const store = writable<HistoryState>({
    canUndo: false,
    canRedo: false,
    undoAction: null,
    redoAction: null,
    processing: false,
  });
  let activeMutation: Promise<HistoryMutationResultV1> | null = null;

  function runMutation(operation: () => Promise<HistoryMutationResultV1>) {
    // A double click or repeated shortcut while the first durable write is pending must observe the
    // same result, never consume a second history entry.
    if (activeMutation) return activeMutation;
    store.update((state) => ({ ...state, processing: true }));
    activeMutation = operation()
      .then((result) => {
        store.set(stateFromStatus(result.status, true));
        return result;
      })
      .finally(() => {
        activeMutation = null;
        store.update((state) => ({ ...state, processing: false }));
      });
    return activeMutation;
  }

  async function refresh() {
    if (!isTauriRuntime()) {
      store.set(stateFromStatus(emptyStatus, false));
      return;
    }

    try {
      store.set(stateFromStatus(await getHistoryStatusV1(), false));
    } catch {
      store.set(stateFromStatus(emptyStatus, false));
    }
  }

  return {
    subscribe: store.subscribe,
    async undo(): Promise<HistoryMutationResultV1> {
      if (!isTauriRuntime()) {
        store.set(stateFromStatus(emptyStatus, false));
        return { action: null, status: emptyStatus };
      }

      return runMutation(undo);
    },
    async redo(): Promise<HistoryMutationResultV1> {
      if (!isTauriRuntime()) {
        store.set(stateFromStatus(emptyStatus, false));
        return { action: null, status: emptyStatus };
      }

      return runMutation(redo);
    },
    refresh,
  };
}

export const historyStore = createHistoryStore();
