import { mount } from 'svelte';
import App from './App.svelte';
import './app.css';

// ---------------------------------------------------------------------------
// Dev-only Tauri IPC mock.
// Lets the Svelte UI render in a plain browser (no Rust backend) so the
// frontend can be previewed and iterated. Completely inert under real Tauri,
// where window.__TAURI_INTERNALS__ already exists.
// ---------------------------------------------------------------------------
if (import.meta.env.DEV && !('__TAURI_INTERNALS__' in window)) {
  const listKinds =
    /(^get_segments$|^list_|_reports$|_events$|_runs$|_history$|^search_|_queue$|^get_speakers$)/;
  const objKinds =
    /(^get_settings$|^app_health$|^db_info$|^import_status$|readiness$|^models_status$|_info$|^get_stats$|^compute_stats$)/;
  const mockInvoke = async (cmd: string): Promise<unknown> => {
    if (cmd.startsWith('plugin:')) return null;
    if (listKinds.test(cmd)) return [];
    if (objKinds.test(cmd)) return {};
    return null;
  };
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
    invoke: (cmd: string) => mockInvoke(cmd),
    transformCallback: (cb: unknown) => {
      const id = Math.floor(Math.random() * 1e9);
      const w = window as unknown as Record<string, Record<number, unknown>>;
      w.__TAURI_CB__ = w.__TAURI_CB__ || {};
      w.__TAURI_CB__[id] = cb;
      return id;
    },
    metadata: {
      currentWindow: { label: 'main' },
      currentWebview: { windowLabel: 'main', label: 'main' },
    },
  };
  // eslint-disable-next-line no-console
  console.info('[cortex] dev Tauri mock installed — UI preview mode (no backend)');
}

const app = mount(App, { target: document.getElementById('app')! });

export default app;
