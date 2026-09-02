import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import path from 'path';
import coverageContract from './scripts/frontend_coverage_contract.v1.json';

const collectingMergedCoverage = process.env.CORTEX_MERGED_COVERAGE === '1';

export default defineConfig({
  plugins: [svelte()],
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./tests/setup.ts'],
    include: ['src/**/*.{test,spec}.{ts,js}', 'tests/**/*.{test,spec}.{ts,js}'],
    coverage: {
      provider: 'v8',
      reporter: ['text', 'json-summary', 'json', 'html'],
      ...(collectingMergedCoverage ? { reportsDirectory: 'coverage/unit' } : {}),
      include: ['src/**/*.{ts,svelte}'],
      exclude: ['src/**/*.d.ts', 'src/**/*.test.*'],
      // Product-certification contract. Keep this fail-closed even while the tree is red: an
      // uncovered workstation cannot inherit a green verdict from a large passing test count.
      ...(collectingMergedCoverage
        ? {}
        : {
            thresholds: {
              ...coverageContract.thresholds,
            },
          }),
    },
  },
  resolve: {
    conditions: ['browser'],
    alias: {
      $lib: path.resolve('./src/lib'),
    },
  },
});
