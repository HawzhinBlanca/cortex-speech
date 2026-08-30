import { defineConfig } from 'vitest/config';

// Deliberately minimal: the owner-critical reducers are plain TypeScript and their three exact
// tests need neither the Svelte transform nor the product-wide coverage threshold. Keeping this
// config independent prevents the mutation sandbox from copying native models, Rust targets,
// browser evidence, or other irrelevant multi-gigabyte trees.
export default defineConfig({
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./tests/setup.ts'],
    include: [
      'tests/lib/audioMachine.test.ts',
      'src/lib/reviewCommitOperation.test.ts',
      'src/lib/reviewCommitResult.test.ts',
    ],
  },
});
