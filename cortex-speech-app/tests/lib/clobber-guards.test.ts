import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

// P2.3 (audit F2): the last two whole-row-clobber paths. These are UI-guard invariants in large Svelte
// components (App.svelte's normalize handler, ReviewMode's unmount teardown) that cannot be exercised by
// a lightweight unit test without mounting the whole heavy component tree — so they are pinned as source
// invariants (the same approach the repo's python policy gates use). A regression to a whole-row upsert /
// an unguarded handler fails these.
const read = (rel: string) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8');

describe('whole-row clobber guards (P2.3 / audit F2)', () => {
  it('the ReviewMode unmount flush persists via the TARGETED field update, never a whole-row updateSegment', () => {
    const src = read('../../src/lib/ReviewMode.svelte');
    const start = src.indexOf('Unmount flush');
    const end = src.indexOf('Transient word-bounded', start);
    expect(start).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(start);
    const flush = src.slice(start, end);
    // Must persist only annotatedTranscript via the field update (leaves alignmentJson untouched)...
    expect(flush).toContain('updateSegmentFields(seg.id, { annotatedTranscript:');
    // ...and must NOT whole-row upsert here (that spread a stale row and reverted a mid-align aligner write).
    expect(flush).not.toMatch(/api\.updateSegment\(/);
  });

  it('handleNormalize refuses while a batch/import is running ($isProcessing guard)', () => {
    const src = read('../../src/App.svelte');
    const start = src.indexOf('async function handleNormalize');
    expect(start).toBeGreaterThan(-1);
    const body = src.slice(start, start + 700);
    expect(body).toContain('if ($isProcessing) return;');
  });
});
