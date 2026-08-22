import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

// Schema-v60 review truth is committed only by the atomic human-decision command. These source pins make
// generic whole-row writes and renderer-owned transcript drafts structurally unreachable.
const read = (rel: string) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8');

describe('schema-v60 frontend review-write boundary', () => {
  it('ReviewMode navigation and unmount never persist renderer-owned transcript drafts', () => {
    const src = read('../../src/lib/ReviewMode.svelte');
    expect(src).toContain('const editCache = new Map<string, string>()');
    expect(src).toContain('session-local editCache');
    expect(src).not.toContain('api.updateSegmentFields(');
    expect(src).not.toMatch(/api\.updateSegment\(/);
  });

  it('the library exposes only a fail-closed metadata partial writer', () => {
    const app = read('../../src/App.svelte');
    const commands = read('../../src/lib/commands.ts');
    expect(commands).not.toContain('export async function updateSegment(');
    expect(commands).toContain("Partial<Pick<SpeechSegment, 'speakerId' | 'alignmentJson'>>");
    expect(app).toContain("key !== 'speakerId' && key !== 'alignmentJson'");
    for (const forbidden of [
      'handleSaveAnnotation',
      'handleToggleVerify',
      'handleNormalize',
      'finishEditingWord',
      'data-testid="verify-btn"',
    ]) {
      expect(app).not.toContain(forbidden);
    }
    expect(app).not.toMatch(/api\.updateSegment\(/);
  });

  it('ReviewMode mutators are clobber-safe and champion re-transcribe reloads the atomic backend commit', () => {
    const src = read('../../src/lib/ReviewMode.svelte');
    const region = (fn: string, next: string) => {
      const s = src.indexOf(fn);
      expect(s, `missing ${fn}`).toBeGreaterThan(-1);
      const e = src.indexOf(next, s + fn.length);
      expect(e, `missing ${next} after ${fn}`).toBeGreaterThan(s);
      return src.slice(s, e);
    };
    // Human decisions are now one backend-owned atomic commit. ReviewMode consumes the authoritative
    // returned row and must not issue a second revision-bumping field write or a stale whole-row upsert.
    const submit = region('async function submit(', 'function advance(');
    expect(submit).toContain('const commit = await api.recordHumanDecision(');
    expect(submit).toContain('commit.segment');
    expect(submit).not.toContain('updateSegmentFields(seg.id');
    expect(submit).not.toMatch(/api\.updateSegment\(/);
    const markBad = region('async function markBad(', 'async function submit(');
    expect(markBad).toContain(
      "const commit = await api.recordHumanDecision(seg.id, 'reject', null)",
    );
    expect(markBad).toContain('commit.segment');
    expect(markBad).not.toContain('updateSegmentFields(seg.id');
    expect(markBad).not.toMatch(/api\.updateSegment\(/);
    const go = region('async function go(', 'function resetToOriginal(');
    expect(go).toContain('session-local editCache');
    expect(go).not.toContain('updateSegmentFields(');
    expect(go).not.toMatch(/api\.updateSegment\(/);
    // Re-transcribe is champion-only: the backend commits transcript + provenance atomically, then the
    // UI reloads that authoritative row. It must never whole-row upsert a stale frontend snapshot.
    const doRetranscribe = region('async function doRetranscribe(', 'async function markBad(');
    expect(doRetranscribe).toMatch(/if \(!seg[^)]*\$isProcessing\) return;/);
    expect(doRetranscribe).toContain(
      'api.transcribeSegment(seg.audioPath, seg.alignmentJson, seg.id)',
    );
    expect(doRetranscribe).toContain('const updated = await api.getSegment(seg.id)');
    expect(doRetranscribe).toContain('updated.modelVersionId');
    expect(doRetranscribe).not.toMatch(/api\.updateSegment\(/);
    expect(doRetranscribe).not.toContain('transcribeSegmentFinetuned');
  });

  it('ReviewInbox uses immutable server effects for both decisions and flags', () => {
    const src = read('../../src/lib/ReviewInbox.svelte');
    expect(src).toContain('effectEventId: commit.effectEventId');
    expect(src).toContain('api.undoHumanDecision(last.effectEventId, last.operationId)');
    expect(src).toContain("api.recordReviewFlag(cur.id, 'Flagged for second-pass adjudication')");
    expect(src).toContain('api.undoReviewFlag(last.effectEventId, last.operationId)');
    expect(src).toMatch(
      /async function undo\(\)[\s\S]*isSubmitting = true;[\s\S]*finally \{\s*isSubmitting = false;/,
    );
    expect(src).not.toContain('api.clearHumanDecision(');
    expect(src).not.toContain('api.clearEscalation(');
  });
});
