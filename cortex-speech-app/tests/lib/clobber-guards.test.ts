import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

// Schema-v60 review truth is committed only by the atomic human-decision command. These source pins make
// generic whole-row writes and renderer-owned transcript drafts structurally unreachable.
const read = (rel: string) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8');

describe('schema-v60 frontend review-write boundary', () => {
  it('ReviewMode navigation and unmount never persist renderer-owned transcript drafts', () => {
    const shell = read('../../src/lib/ReviewMode.svelte');
    const draft = read('../../src/lib/reviewModeDraft.svelte.ts');
    expect(draft).toContain('const editCache = new Map<string, string>()');
    expect(draft).toContain('editCache.set(');
    expect(draft).toContain('queueWrite(');
    expect(`${shell}\n${draft}`).not.toContain('api.updateSegmentMetadataV1(');
    expect(`${shell}\n${draft}`).not.toMatch(/api\.updateSegment\(/);
  });

  it('the library exposes only a fail-closed metadata partial writer', () => {
    const app = read('../../src/Workstation.svelte');
    const commands = read('../../src/lib/commands.ts');
    const coordinator = read('../../src/lib/segmentMetadataCoordinator.ts');
    expect(commands).not.toContain('export async function updateSegment(');
    expect(commands).toContain("Pick<SpeechSegment, 'speakerId' | 'alignmentJson'>");
    expect(commands).toContain('export async function updateSegmentMetadataV1(');
    expect(commands).toContain('expected: SegmentMetadataBaseline');
    expect(coordinator).toContain("key !== 'speakerId' && key !== 'alignmentJson'");
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

  it('flushes intersecting metadata before deletion instead of cancelling a failed save', () => {
    const app = read('../../src/Workstation.svelte');
    const segmentActions = read('../../src/lib/workstationSegmentActions.ts');
    const batchActions = read('../../src/lib/workstationBatchActions.ts');
    expect(app).toContain('flushAutosaveForIds(autosave, ids)');
    expect(segmentActions).toContain('await flushAutosaveIds([segment.id])');
    expect(batchActions).toContain('await flushAutosave(ids)');
    expect(`${app}\n${segmentActions}\n${batchActions}`).not.toContain('cancelPendingSave');
  });

  it('ReviewMode mutators are clobber-safe and champion re-transcribe reloads the atomic backend commit', () => {
    const shell = read('../../src/lib/ReviewMode.svelte');
    const decisions = read('../../src/lib/reviewModeDecisions.svelte.ts');
    const draft = read('../../src/lib/reviewModeDraft.svelte.ts');
    const region = (source: string, fn: string, next: string) => {
      const s = source.indexOf(fn);
      expect(s, `missing ${fn}`).toBeGreaterThan(-1);
      const e = source.indexOf(next, s + fn.length);
      expect(e, `missing ${next} after ${fn}`).toBeGreaterThan(s);
      return source.slice(s, e);
    };
    // Human decisions are now one versioned backend-owned atomic commit. ReviewMode reloads the
    // authoritative row and must not issue a second revision-bumping field write or stale upsert.
    const submit = region(decisions, 'async function submit(', 'async function go(');
    expect(submit).toContain('const commit = await api.commitReviewV1({');
    expect(submit).toContain('baseRevision');
    expect(submit).toContain('const decisionOperationId = commitOperations.idFor(intent)');
    expect(submit).toContain('effectEventId: effectId');
    expect(submit).toContain('decisionOperationId');
    expect(submit).toContain('await settleTruthProjection(truthLease');
    expect(submit).not.toContain('commit.authoritativeTranscript');
    expect(submit).not.toContain('updateSegmentMetadataV1(seg.id');
    expect(submit).not.toMatch(/api\.updateSegment\(/);
    const markBad = region(decisions, 'async function markBad(', 'function validUnusableResponse(');
    expect(markBad).toContain('const commit = await api.commitReviewV1({');
    expect(markBad).toContain("decision: 'reject'");
    expect(markBad).toContain('baseRevision');
    expect(markBad).toContain('const decisionOperationId = commitOperations.idFor(intent)');
    expect(markBad).toContain('effectEventId: effectId');
    expect(markBad).toContain('decisionOperationId');
    expect(markBad).toContain('await settleTruthProjection(truthLease');
    expect(markBad).not.toContain('commit.authoritativeTranscript');
    expect(markBad).not.toContain('updateSegmentMetadataV1(seg.id');
    expect(markBad).not.toMatch(/api\.updateSegment\(/);
    const go = region(decisions, 'async function go(', '\n  return {');
    expect(go).toContain('const targetId = row.id');
    expect(go).toContain('await deps.queue.hydrate(targetId)');
    expect(go).not.toContain('updateSegmentMetadataV1(');
    expect(go).not.toMatch(/api\.updateSegment\(/);
    expect(draft).toContain('const editCache = new Map<string, string>()');
    expect(draft).toContain('void queueWrite(');
    // Re-transcribe is champion-only: the backend commits transcript + provenance atomically, then the
    // UI reloads that authoritative row. It must never whole-row upsert a stale frontend snapshot.
    const doRetranscribe = region(shell, 'async function doRetranscribe(', '  // Cloud watcher:');
    expect(doRetranscribe).toMatch(/if \([^)]*!seg[^)]*\$isProcessing\) return;/);
    expect(doRetranscribe).toContain(
      'api.transcribeSegment(seg.audioPath, seg.alignmentJson, seg.id)',
    );
    expect(doRetranscribe).toContain('const updated = await api.getSegment(seg.id)');
    expect(doRetranscribe).toContain('updated.modelVersionId');
    expect(doRetranscribe).not.toContain('ensureWordTimings(updated)');
    expect(doRetranscribe).not.toContain('api.alignSegment(');
    expect(doRetranscribe).not.toMatch(/api\.updateSegment\(/);
    expect(doRetranscribe).not.toContain('transcribeSegmentFinetuned');

    // Timing evidence is committed or invalidated inside the same backend transaction as the new
    // transcript. Neither workstation surface may append an independent alignment write afterward,
    // because that would make the just-recorded exact Undo endpoint stale.
    const workstation = read('../../src/lib/workstationSegmentActions.ts');
    const transcribeStart = workstation.indexOf('async function transcribe()');
    const transcribeEnd = workstation.indexOf('async function saveSpeaker()', transcribeStart);
    expect(transcribeStart).toBeGreaterThan(-1);
    expect(transcribeEnd).toBeGreaterThan(transcribeStart);
    const handleTranscribe = workstation.slice(transcribeStart, transcribeEnd);
    expect(handleTranscribe).toContain(
      'api.transcribeSegment(segment.audioPath, segment.alignmentJson, segment.id)',
    );
    expect(handleTranscribe).not.toContain('api.alignSegment(');
    const alignStart = workstation.indexOf('async function align()');
    const alignEnd = workstation.indexOf('\n  return {', alignStart);
    expect(alignStart).toBeGreaterThan(-1);
    expect(alignEnd).toBeGreaterThan(alignStart);
    const handleAlign = workstation.slice(alignStart, alignEnd);
    expect(handleAlign).toContain('const text = effectiveTranscript(segment);');
    expect(handleAlign).toContain(
      'segment.audioPath,\n        text,\n        segment.alignmentJson,\n        segment.id,',
    );
  });

  it('both workstations use one tagged exact desktop review-action Undo authority', () => {
    const inbox = read('../../src/lib/reviewInboxDecisions.svelte.ts');
    const mode = read('../../src/lib/reviewModeDecisions.svelte.ts');
    const commands = read('../../src/lib/commands.ts');
    const durableUndo = read('../../src/lib/durableReviewUndo.svelte.ts');
    const legacy = read('../../src/lib/adapters/legacyIpc.ts');
    const e2eMock = read('../../e2e/helpers/tauri-mock.ts');
    for (const src of [inbox, mode]) {
      expect(src).toMatch(
        /api\.undoDesktopReviewActionV1\(\s*actionRequest\.target,\s*actionRequest\.operationId,?\s*\)/,
      );
      expect(src).toContain('validatedDesktopReviewUndoOutcome(rawOutcome, actionRequest.target)');
      expect(src).toContain('durableUndo.requireProjectionReload(');
      expect(src).toContain('await durableUndo.reconcileProjections(segments)');
    }
    expect(inbox).toContain('request.operationId,');
    expect(inbox).toContain('commit.effectEventId');
    expect(inbox).toContain("case 'flagShadowed':");
    expect(inbox).toContain("return 'review.undoDisabled.flagShadowed';");
    expect(inbox).toMatch(
      /async function undo\(\)[\s\S]*state\.submitting = true;[\s\S]*finally \{\s*state\.submitting = false;/,
    );
    expect(commands).toContain('export async function getDesktopReviewUndoAvailabilityV1()');
    expect(commands).toContain('export async function undoDesktopReviewActionV1(');
    expect(commands).toContain(
      'const request: UndoDesktopReviewRequestV1 = { target, operationId };',
    );
    expect(durableUndo).toContain('outcome.effectKind !== target.kind');
    expect(e2eMock).toContain("case 'get_desktop_review_undo_target_v1':");
    expect(e2eMock).toContain("case 'undo_desktop_review_action_v1':");
    expect(e2eMock).toContain('Malformed complete desktop Undo target in E2E request');
    const productionUndoBoundary = `${inbox}\n${mode}\n${commands}\n${legacy}`;
    for (const retired of [
      'undoHumanDecision',
      'undoReviewFlag',
      'undo_human_decision',
      'undo_review_flag',
      'api.clearHumanDecision(',
      'api.clearEscalation(',
    ]) {
      expect(productionUndoBoundary).not.toContain(retired);
      expect(e2eMock).not.toContain(retired);
    }
  });
});
