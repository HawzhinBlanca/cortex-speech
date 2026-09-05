import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

const host = process.env.TAURI_DEV_HOST;

export default defineConfig(async () => ({
  plugins: [svelte()],
  clearScreen: false,
  optimizeDeps: {
    // Scan lazy workspaces before the first browser request too. HTML-only discovery missed
    // a deep icon import and returned 504 "Outdated Optimize Dep" during a cold overflow open.
    // This affects dev pre-bundling only; production workspaces remain dynamically loaded.
    entries: ['index.html', 'src/**/*.svelte', 'src/**/*.ts'],
  },
  build: {
    // The checked-in budget gate walks this graph and counts every transitive static dependency.
    // Dynamic workspaces remain excluded until the user opens them.
    manifest: true,
  },
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: 'ws',
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },
}));
