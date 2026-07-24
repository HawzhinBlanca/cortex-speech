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

  it('the 4 ReviewMode mutators are clobber-safe: submit/markBad/go use targeted field updates; doRetranscribe refuses a batch (P2.3b)', () => {
    const src = read('../../src/lib/ReviewMode.svelte');
    const region = (fn: string, next: string) => {
      const s = src.indexOf(fn);
      expect(s, `missing ${fn}`).toBeGreaterThan(-1);
      const e = src.indexOf(next, s + fn.length);
      expect(e, `missing ${next} after ${fn}`).toBeGreaterThan(s);
      return src.slice(s, e);
    };
    // submit / markBad / go persist only whitelisted fields via the targeted update — never a whole-row
    // upsert of the batch-stale store row.
    const submit = region('async function submit(', 'function advance(');
    expect(submit).toContain('updateSegmentFields(seg.id, { annotatedTranscript: text, verified: true })');
    expect(submit).not.toMatch(/api\.updateSegment\(/);
    const markBad = region('async function markBad(', 'async function submit(');
    expect(markBad).toContain('updateSegmentFields(seg.id, { verified: true })');
    expect(markBad).not.toMatch(/api\.updateSegment\(/);
    const go = region('async function go(', 'function resetToOriginal(');
    expect(go).toContain('updateSegmentFields(seg.id, { annotatedTranscript: text })');
    expect(go).not.toMatch(/api\.updateSegment\(/);
    // doRetranscribe writes rawTranscript (not whitelisted) so it stays a whole-row upsert, but must be
    // refused during a batch/import.
    const doRetranscribe = region('async function doRetranscribe(', 'async function markBad(');
    expect(doRetranscribe).toMatch(/if \(!seg[^)]*\$isProcessing\) return;/);
  });
});
