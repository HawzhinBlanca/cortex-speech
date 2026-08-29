import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { get } from 'svelte/store';
import ReviewMode from '../../src/lib/ReviewMode.svelte';
import { searchQuery, segments, selectedSegmentId } from '../../src/lib/stores/segmentStore';
import { defaultSettings, settings } from '../../src/lib/stores/settingsStore';
import { showConfirmDialog, showReviewInbox } from '../../src/lib/stores/uiStore';
import type { SpeechSegment } from '../../src/lib/types';
import { ckb } from '../../src/lib/i18n/ckb';
import { flushReviewDrafts } from '../../src/lib/reviewDraftFlush';
import { sharedDurableReviewUndo } from '../../src/lib/durableReviewUndo.svelte';
import { createReviewModeQueueController } from '../../src/lib/reviewModeQueue.svelte';

const MEDIA_GRANT_ID = '2f2d9b66-8566-4d1c-8c14-e18d006b776f';
const NEXT_MEDIA_GRANT_ID = '52a492d4-14d8-4e24-9f5d-bc44221b48c1';
const UNDO_PAYLOAD_HASH = 'a'.repeat(64);

type UndoDecision = 'accept' | 'edit' | 'reject';
type DecisionUndoTarget = {
  kind: 'decision';
  effectEventId: number;
  segmentId: string;
  decision: UndoDecision;
  sourceOperationId: string;
  sourcePayloadHash: string;
  databaseGeneration: number;
};
type FlagUndoTarget = {
  kind: 'flag';
  effectEventId: number;
  segmentId: string;
  sourceOperationId: string;
  sourcePayloadHash: string;
  priorRevision: number;
  flagRevision: number;
  flagKind: {
    kind: 'technicalUnusable';
    reason: 'decodeFailed' | 'missingFile' | 'permissionDenied' | 'corruptContainer';
  };
  databaseGeneration: number;
};
type UndoTarget = DecisionUndoTarget | FlagUndoTarget;
type UndoAvailability =
  | { status: 'available'; target: UndoTarget }
  | { status: 'none' }
  | {
      status: 'blocked';
      reason:
        | 'legacyHistory'
        | 'latestDecisionUndone'
        | 'latestFlagUndone'
        | 'decisionShadowed'
        | 'flagShadowed';
    };

let undoAvailability: UndoAvailability;

function undoTarget(
  segmentId: string,
  decision: UndoDecision,
  sourceOperationId: string,
  effectEventId = 101,
): DecisionUndoTarget {
  return {
    kind: 'decision',
    effectEventId,
    segmentId,
    decision,
    sourceOperationId: sourceOperationId,
    sourcePayloadHash: UNDO_PAYLOAD_HASH,
    databaseGeneration: 1,
  };
}

function technicalFlagUndoTarget(
  segmentId: string,
  sourceOperationId: string,
  priorRevision: number,
  reason: FlagUndoTarget['flagKind']['reason'],
  effectEventId = 202,
): FlagUndoTarget {
  return {
    kind: 'flag',
    effectEventId,
    segmentId,
    sourceOperationId,
    sourcePayloadHash: 'c'.repeat(64),
    priorRevision,
    flagRevision: priorRevision + 1,
    flagKind: { kind: 'technicalUnusable', reason },
    databaseGeneration: 1,
  };
}

const mocks = vi.hoisted(() => ({
  getSegmentsPage: vi.fn(),
  getReviewPageV1: vi.fn(),
  getSegment: vi.fn(),
  getSegmentConsensus: vi.fn(),
  getDatasetStats: vi.fn(),
  getDatasetCertificate: vi.fn(),
  getWaveform: vi.fn(),
  alignSegment: vi.fn(),
  transcribeSegment: vi.fn(),
  recordPlaybackReceipt: vi.fn(),
  recordHumanDecision: vi.fn(),
  commitReviewV1: vi.fn(),
  markSegmentUnusableV1: vi.fn(),
  getReviewDraftV1: vi.fn(),
  saveReviewDraftV1: vi.fn(),
  deleteReviewDraftV1: vi.fn(),
  getDesktopReviewUndoAvailabilityV1: vi.fn(),
  undoDesktopReviewActionV1: vi.fn(),
  updateSegmentMetadataV1: vi.fn(),
  registerMediaAsset: vi.fn(),
  registerReviewMediaAsset: vi.fn(),
  getMediaAssetUrl: vi.fn(),
  beginDesktopPlaybackSessionV1: vi.fn(),
  cancelDesktopPlaybackSessionV1: vi.fn(),
}));

vi.mock('../../src/lib/commands', () => ({
  ...mocks,
  is7bUnavailableError: vi.fn(() => false),
  isCommandErrorV1: vi.fn(
    (error: unknown, code?: string) =>
      !!error &&
      typeof error === 'object' &&
      (error as { schema?: number }).schema === 1 &&
      (code === undefined || (error as { code?: string }).code === code),
  ),
  reviewEffectId: vi.fn((decisionId: string) => {
    const match = /^effect:([1-9][0-9]*)$/.exec(decisionId);
    return match ? Number(match[1]) : null;
  }),
  reviewErrorMessage: vi.fn((_error: unknown, fallback: string) => fallback),
  engineLabel: vi.fn((id: string) => id),
}));

function segment(): SpeechSegment {
  return {
    id: 'review-1',
    createdAt: '2026-08-15T00:00:00Z',
    audioPath: 'C:\\audio\\review.wav',
    rawTranscript: 'دەقی تاقیکردنەوە',
    normalizedTranscript: null,
    annotatedTranscript: null,
    alignmentJson: JSON.stringify({
      source_start_ms: 0,
      source_end_ms: 1_000,
      words: [{ word: 'دەقی', start: 0, end: 0.4, confidence: 0.9 }],
    }),
    alignmentQuality: 'ctc_forced',
    durationMs: 1000,
    speakerId: null,
    verified: false,
    evidenceJson: null,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function installPlayableMediaStub() {
  Object.defineProperty(HTMLMediaElement.prototype, 'paused', {
    configurable: true,
    get(this: HTMLMediaElement & { __paused?: boolean }) {
      return this.__paused !== false;
    },
  });
  HTMLMediaElement.prototype.play = function (this: HTMLMediaElement & { __paused?: boolean }) {
    this.__paused = false;
    return Promise.resolve();
  };
  HTMLMediaElement.prototype.pause = function (this: HTMLMediaElement & { __paused?: boolean }) {
    this.__paused = true;
  };
  HTMLMediaElement.prototype.load = vi.fn();
}

let activePlaybackRevision = 0;
let removedReviewSegmentIds: Set<string>;

async function hearCurrentAudio() {
  await waitFor(() => expect(sharedDurableReviewUndo.state.status).not.toBe('loading'));
  const audio = document.querySelector('audio');
  expect(audio).not.toBeNull();
  await waitFor(() => expect(audio!.getAttribute('src')).toBeTruthy());
  Object.defineProperty(audio!, 'duration', { configurable: true, value: 1 });
  audio!.currentTime = 0;
  await fireEvent.loadedMetadata(audio!);
  const play = await waitFor(() => {
    const button = document.querySelector<HTMLButtonElement>(
      '[data-testid="audio-player-timeline"] button',
    );
    expect(button).not.toBeNull();
    return button!;
  });
  await fireEvent.click(play);
  await waitFor(() => expect(audio!.paused).toBe(false));
  audio!.currentTime = 0.9;
  await fireEvent.timeUpdate(audio!);
  await Promise.resolve();
}

function decisionCommit(
  source: SpeechSegment,
  action: 'accept' | 'edit' | 'reject',
  text?: string | null,
) {
  return {
    effectEventId: 101,
    segmentId: source.id,
    effectiveAction: action,
    priorRevision: 0,
    decidedRevision: 1,
    segment: {
      ...source,
      humanDecision: action,
      verdict: `human_${action}`,
      verdictTranscript: text ?? source.verdictTranscript ?? source.rawTranscript,
      annotatedTranscript: text ?? source.annotatedTranscript,
      verified: true,
    },
  };
}

describe('ReviewMode windowed queue', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    Object.assign(sharedDurableReviewUndo.state, {
      status: 'loading',
      target: null,
      operationId: null,
      blockedReason: null,
      errorCode: null,
      inFlight: false,
      truthWriteInFlight: false,
      truthWriteAmbiguous: false,
      truthProjectionPending: false,
      projectionOutcome: null,
    });
    undoAvailability = { status: 'none' };
    removedReviewSegmentIds = new Set();
    segments.set([]);
    selectedSegmentId.set(null);
    searchQuery.set('');
    settings.set({ ...defaultSettings });
    showReviewInbox.set(false);
    showConfirmDialog.set(null);
    activePlaybackRevision = 0;
    mocks.cancelDesktopPlaybackSessionV1.mockResolvedValue(true);
    mocks.getSegmentConsensus.mockResolvedValue({ models: [], words: [] });
    mocks.getDatasetStats.mockResolvedValue({ totalSegments: 1, verifiedCount: 0 });
    mocks.getDatasetCertificate.mockResolvedValue({ threshold: 0.35 });
    mocks.getWaveform.mockResolvedValue([0.1, 0.4, 0.2]);
    mocks.transcribeSegment.mockResolvedValue(undefined);
    mocks.recordPlaybackReceipt.mockImplementation(async () => ({
      playbackReceiptId: '11111111-1111-4111-8111-111111111111',
      segmentId: 'review-1',
      segmentRevision: activePlaybackRevision,
      uniquePlayedMs: 1000,
      clipDurationMs: 1000,
      coverageRatio: 1,
    }));
    mocks.beginDesktopPlaybackSessionV1.mockImplementation(
      async (segmentId: string, _mediaGrantId: string, expectedRevision: number) => {
        activePlaybackRevision = expectedRevision;
        return {
          playbackReceiptId: '11111111-1111-4111-8111-111111111111',
          segmentId,
          segmentRevision: expectedRevision,
          clipDurationMs: 1000,
          expiresAtMs: Date.now() + 60_000,
        };
      },
    );
    mocks.recordHumanDecision.mockImplementation(
      async (id: string, action: 'accept' | 'edit' | 'reject', text?: string | null) =>
        decisionCommit({ ...segment(), id }, action, text),
    );
    mocks.getReviewPageV1.mockImplementation(
      async (scope: { kind: string; query?: string }, cursor: string | null, limit: number) => {
        const page = await mocks.getSegmentsPage({
          verified: false,
          query: scope.kind === 'search' ? scope.query : null,
          sort: 'oldest',
          limit,
          cursor,
          focused: true,
        });
        const projected = page.items.filter(
          (item: SpeechSegment) => !removedReviewSegmentIds.has(item.id),
        );
        return {
          items: projected.map((item: SpeechSegment) => ({
            segment: item,
            baseRevision: page.revisions?.[item.id] ?? 0,
            eligible: true,
            disabledReason: null,
          })),
          total: Math.max(0, page.total - (page.items.length - projected.length)),
          nextCursor: page.nextCursor,
          scopeLabel: scope.kind,
          focusNarrowed: page.focusNarrowed === true,
        };
      },
    );
    mocks.commitReviewV1.mockImplementation(
      async (request: {
        operationId: string;
        segmentId: string;
        decision: UndoDecision;
        transcript: string | null;
      }) => {
        const legacy = await mocks.recordHumanDecision(
          request.segmentId,
          request.decision,
          request.transcript,
        );
        removedReviewSegmentIds.add(request.segmentId);
        undoAvailability = {
          status: 'available',
          target: undoTarget(
            request.segmentId,
            request.decision,
            request.operationId,
            legacy.effectEventId,
          ),
        };
        return {
          segmentId: legacy.segmentId,
          committedRevision: legacy.decidedRevision,
          authoritativeTranscript: legacy.segment.verdictTranscript ?? legacy.segment.rawTranscript,
          decisionId: `effect:${legacy.effectEventId}`,
        };
      },
    );
    mocks.markSegmentUnusableV1.mockImplementation(
      async (request: {
        operationId: string;
        segmentId: string;
        baseRevision: number;
        reason: 'decodeFailed' | 'missingFile' | 'permissionDenied' | 'corruptContainer';
      }) => {
        removedReviewSegmentIds.add(request.segmentId);
        undoAvailability = {
          status: 'available',
          target: technicalFlagUndoTarget(
            request.segmentId,
            request.operationId,
            request.baseRevision,
            request.reason,
          ),
        };
        return {
          segmentId: request.segmentId,
          committedRevision: request.baseRevision + 1,
          reason: request.reason,
          effectId: 'flag-effect:202',
        };
      },
    );
    mocks.getReviewDraftV1.mockResolvedValue(null);
    mocks.saveReviewDraftV1.mockImplementation(
      async (segmentId: string, baseRevision: number, text: string) => ({
        segmentId,
        baseRevision,
        text,
        updatedAt: '2026-08-25T12:00:00.000Z',
      }),
    );
    mocks.deleteReviewDraftV1.mockResolvedValue(true);
    mocks.getDesktopReviewUndoAvailabilityV1.mockImplementation(async () => undoAvailability);
    mocks.undoDesktopReviewActionV1.mockImplementation(async (target: UndoTarget) => {
      removedReviewSegmentIds.delete(target.segmentId);
      undoAvailability = {
        status: 'blocked',
        reason: target.kind === 'flag' ? 'latestFlagUndone' : 'latestDecisionUndone',
      };
      return {
        status: 'applied',
        effectKind: target.kind,
        effectEventId: target.effectEventId,
        restoredRevision: 2,
        segment: { ...segment(), id: target.segmentId },
      };
    });
    mocks.updateSegmentMetadataV1.mockResolvedValue(undefined);
    mocks.registerMediaAsset.mockResolvedValue({ id: MEDIA_GRANT_ID });
    mocks.registerReviewMediaAsset.mockResolvedValue({ id: MEDIA_GRANT_ID });
    mocks.getMediaAssetUrl.mockImplementation(
      async (id: string) => `http://cortex-media.localhost/${id}`,
    );
    installPlayableMediaStub();
    Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', {
      configurable: true,
      value: vi.fn(() => null),
    });
  });

  it('shows a truthful terminal only after the pending-page request returns empty', async () => {
    mocks.getSegmentsPage.mockResolvedValue({ items: [], total: 0, nextCursor: null });
    render(ReviewMode);
    expect(await screen.findByTestId('review-terminal')).toBeInTheDocument();
    expect(mocks.getSegmentsPage).toHaveBeenCalledWith(
      expect.objectContaining({ verified: false, limit: 100, cursor: null }),
    );
  });

  it('shows a retryable error instead of claiming the queue is complete when loading fails', async () => {
    mocks.getSegmentsPage
      .mockRejectedValueOnce(new Error('database unavailable'))
      .mockResolvedValueOnce({ items: [], total: 0, nextCursor: null });

    render(ReviewMode);
    const errorState = await screen.findByTestId('review-load-error');
    expect(errorState).toHaveTextContent(ckb['errors.unknown']);
    expect(errorState).not.toHaveTextContent('database unavailable');
    expect(screen.queryByTestId('review-terminal')).not.toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: ckb.retry }));
    expect(await screen.findByTestId('review-terminal')).toBeInTheDocument();
    expect(mocks.getSegmentsPage).toHaveBeenCalledTimes(2);
  });

  it('hydrates only the current row and removes a persisted rejection from the queue', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
    });
    mocks.getSegment.mockResolvedValue(full);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(mocks.getSegment).toHaveBeenCalledTimes(1);
    await hearCurrentAudio();

    await fireEvent.click(screen.getByRole('button', { name: ckb['review.markBad'] }));
    await waitFor(() =>
      expect(mocks.recordHumanDecision).toHaveBeenCalledWith('review-1', 'reject', null),
    );
    expect(mocks.updateSegmentMetadataV1).not.toHaveBeenCalled();
    expect(await screen.findByTestId('review-terminal')).toBeInTheDocument();
  });

  it('undoes by immutable effect id and applies only the authoritative server row', async () => {
    const original = segment();
    const restored = { ...original, rawTranscript: 'authoritative restored text' };
    let projectionRows: SpeechSegment[] = [
      { ...original, alignmentJson: null, evidenceJson: null },
    ];
    let authoritativeRow = original;
    mocks.getSegmentsPage.mockImplementation(async () => ({
      items: projectionRows,
      total: projectionRows.length,
      nextCursor: null,
    }));
    mocks.getSegment.mockImplementation(async () => authoritativeRow);
    mocks.recordHumanDecision.mockImplementation(async (id, action, text) => {
      projectionRows = [];
      return decisionCommit({ ...original, id }, action, text);
    });
    mocks.undoDesktopReviewActionV1.mockImplementation(async (target: UndoTarget) => {
      authoritativeRow = restored;
      projectionRows = [{ ...restored, alignmentJson: null, evidenceJson: null }];
      removedReviewSegmentIds.delete(target.segmentId);
      undoAvailability = { status: 'blocked', reason: 'latestDecisionUndone' };
      return {
        status: 'applied',
        effectKind: target.kind,
        effectEventId: target.effectEventId,
        restoredRevision: 2,
        segment: restored,
      };
    });

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: ckb['review.markBad'] }));
    expect(await screen.findByTestId('review-terminal')).toBeInTheDocument();
    await waitFor(() => expect(sharedDurableReviewUndo.state.status).toBe('ready'));
    expect(sharedDurableReviewUndo.state.target).toEqual(
      expect.objectContaining({ effectEventId: 101, segmentId: original.id, decision: 'reject' }),
    );

    await fireEvent.keyDown(window, { key: 'Backspace', code: 'Backspace' });
    await waitFor(() => expect(mocks.undoDesktopReviewActionV1).toHaveBeenCalledTimes(1));
    const [target, operationId] = mocks.undoDesktopReviewActionV1.mock.calls[0];
    expect(target).toEqual({
      kind: 'decision',
      effectEventId: 101,
      segmentId: original.id,
      decision: 'reject',
      sourceOperationId: expect.stringMatching(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
      ),
      sourcePayloadHash: UNDO_PAYLOAD_HASH,
      databaseGeneration: 1,
    });
    expect(operationId).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toHaveValue('authoritative restored text');
    expect(mocks.updateSegmentMetadataV1).not.toHaveBeenCalled();
  });

  it('retries an ambiguous undo with the exact same inverse operation', async () => {
    const original = segment();
    const restored = { ...original, rawTranscript: 'restored after retry' };
    let projectionRows: SpeechSegment[] = [
      { ...original, alignmentJson: null, evidenceJson: null },
    ];
    let authoritativeRow = original;
    mocks.getSegmentsPage.mockImplementation(async () => ({
      items: projectionRows,
      total: projectionRows.length,
      nextCursor: null,
    }));
    mocks.getSegment.mockImplementation(async () => authoritativeRow);
    mocks.recordHumanDecision.mockImplementation(async (id, action, text) => {
      projectionRows = [];
      return decisionCommit({ ...original, id }, action, text);
    });
    mocks.undoDesktopReviewActionV1
      .mockRejectedValueOnce(new Error('response lost after durable inverse'))
      .mockImplementationOnce(async (target: UndoTarget) => {
        authoritativeRow = restored;
        projectionRows = [{ ...restored, alignmentJson: null, evidenceJson: null }];
        removedReviewSegmentIds.delete(target.segmentId);
        undoAvailability = { status: 'blocked', reason: 'latestDecisionUndone' };
        return {
          status: 'applied',
          effectKind: target.kind,
          effectEventId: target.effectEventId,
          restoredRevision: 2,
          segment: restored,
        };
      });

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: ckb['review.markBad'] }));
    expect(await screen.findByTestId('review-terminal')).toBeInTheDocument();
    await waitFor(() => expect(sharedDurableReviewUndo.state.status).toBe('ready'));

    await fireEvent.keyDown(window, { key: 'Backspace', code: 'Backspace' });
    await waitFor(() => expect(mocks.undoDesktopReviewActionV1).toHaveBeenCalledTimes(1));
    const first = mocks.undoDesktopReviewActionV1.mock.calls[0];
    expect(screen.getByTestId('review-terminal')).toBeInTheDocument();

    await fireEvent.keyDown(window, { key: 'Backspace', code: 'Backspace' });
    await waitFor(() => expect(mocks.undoDesktopReviewActionV1).toHaveBeenCalledTimes(2));
    expect(mocks.undoDesktopReviewActionV1.mock.calls[1]).toEqual(first);
    expect(first[0]).toEqual(
      undoTarget(original.id, 'reject', first[0].sourceOperationId, first[0].effectEventId),
    );
    expect(first[1]).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toHaveValue('restored after retry');
  });

  it('keeps the current review selected when navigation hydration fails', async () => {
    const first = segment();
    const second = {
      ...segment(),
      id: 'review-2',
      audioPath: 'C:\\audio\\review-2.wav',
      rawTranscript: 'second transcript',
    };
    mocks.getSegmentsPage.mockResolvedValue({
      items: [
        { ...first, alignmentJson: null, evidenceJson: null },
        { ...second, alignmentJson: null, evidenceJson: null },
      ],
      total: 2,
      nextCursor: null,
    });
    mocks.getSegment.mockImplementation(async (id: string) => {
      if (id === first.id) return first;
      throw new Error('second clip hydration failed');
    });

    render(ReviewMode);
    expect(await screen.findByRole('textbox')).toHaveValue(first.rawTranscript);
    await fireEvent.keyDown(window, { key: 'ArrowRight', code: 'ArrowRight' });

    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledWith(second.id));
    expect(screen.getByRole('textbox')).toHaveValue(first.rawTranscript);
    expect(screen.getByTestId('review-source-file')).toHaveTextContent('review.wav');
  });

  it('drops a navigation hydration when durable truth authority starts before it resolves', async () => {
    const first = segment();
    const second = {
      ...segment(),
      id: 'review-2',
      audioPath: 'C:\\audio\\review-2.wav',
      rawTranscript: 'second transcript',
    };
    const lateSecond = deferred<SpeechSegment>();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [
        { ...first, alignmentJson: null, evidenceJson: null },
        { ...second, alignmentJson: null, evidenceJson: null },
      ],
      total: 2,
      nextCursor: null,
      revisions: { [first.id]: 0, [second.id]: 0 },
    });
    mocks.getSegment.mockImplementation((id: string) =>
      id === first.id ? Promise.resolve(first) : lateSecond.promise,
    );

    render(ReviewMode);
    expect(await screen.findByRole('textbox')).toHaveValue(first.rawTranscript);
    await fireEvent.keyDown(window, { key: 'ArrowRight', code: 'ArrowRight' });
    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledWith(second.id));

    Object.assign(sharedDurableReviewUndo.state, {
      status: 'failed',
      truthWriteInFlight: false,
      truthWriteAmbiguous: true,
      truthProjectionPending: false,
    });
    lateSecond.resolve(second);
    await lateSecond.promise;
    await Promise.resolve();

    expect(screen.getByRole('textbox')).toHaveValue(first.rawTranscript);
    expect(screen.getByTestId('review-source-file')).toHaveTextContent('review.wav');
  });

  it('lets a newer stay-on-current intent cancel an older in-flight next hydration', async () => {
    const first = segment();
    const second = {
      ...segment(),
      id: 'review-2',
      audioPath: 'C:\\audio\\review-2.wav',
      rawTranscript: 'second transcript',
    };
    const lateSecond = deferred<SpeechSegment>();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [
        { ...first, alignmentJson: null, evidenceJson: null },
        { ...second, alignmentJson: null, evidenceJson: null },
      ],
      total: 2,
      nextCursor: null,
      revisions: { [first.id]: 0, [second.id]: 0 },
    });
    mocks.getSegment.mockImplementation((id: string) =>
      id === first.id ? Promise.resolve(first) : lateSecond.promise,
    );

    render(ReviewMode);
    expect(await screen.findByRole('textbox')).toHaveValue(first.rawTranscript);
    await fireEvent.keyDown(window, { key: 'ArrowRight', code: 'ArrowRight' });
    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledWith(second.id));
    await fireEvent.keyDown(window, { key: 'ArrowLeft', code: 'ArrowLeft' });
    lateSecond.resolve(second);
    await lateSecond.promise;
    await Promise.resolve();

    expect(screen.getByRole('textbox')).toHaveValue(first.rawTranscript);
    expect(screen.getByTestId('review-source-file')).toHaveTextContent('review.wav');
  });

  it('never exposes a lightweight row before its chunk metadata is hydrated', async () => {
    const full = {
      ...segment(),
      alignmentJson: JSON.stringify({
        source_start_ms: 12_000,
        source_end_ms: 13_000,
        chunk_index: 2,
        chunk_count: 4,
        words: [{ word: 'دەقی', start: 0, end: 0.4, confidence: 0.9 }],
      }),
    };
    let resolveHydration!: (value: SpeechSegment) => void;
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
    });
    mocks.getSegment.mockReturnValue(
      new Promise<SpeechSegment>((resolve) => {
        resolveHydration = resolve;
      }),
    );

    render(ReviewMode);
    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledWith('review-1'));
    expect(screen.queryByTestId('review-action-bar')).not.toBeInTheDocument();
    expect(mocks.alignSegment).not.toHaveBeenCalled();

    resolveHydration(full);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(mocks.alignSegment).not.toHaveBeenCalled();
  });

  it('keeps the current hydration receipt when a retired same-row hydration resolves last', async () => {
    const rowId = 'same-row-reloaded';
    const hydrationOne = deferred<SpeechSegment>();
    const hydrationTwo = deferred<SpeechSegment>();
    let pageCall = 0;
    let hydrationCall = 0;
    mocks.getReviewPageV1.mockImplementation(async () => {
      pageCall += 1;
      const baseRevision = pageCall === 1 ? 1 : 2;
      return {
        items: [
          {
            segment: {
              ...segment(),
              id: rowId,
              speakerId: pageCall === 1 ? 'page-one' : 'page-two',
            },
            baseRevision,
            eligible: true,
            disabledReason: null,
          },
        ],
        total: 1,
        nextCursor: null,
        scopeLabel: 'pending',
        focusNarrowed: false,
      };
    });
    mocks.getSegment.mockImplementation(async () => {
      hydrationCall += 1;
      if (hydrationCall === 1) return hydrationOne.promise;
      if (hydrationCall === 2) return hydrationTwo.promise;
      return { ...segment(), id: rowId, speakerId: 'current-authority' };
    });

    const queue = createReviewModeQueueController();
    await expect(queue.load(true)).resolves.toBe(true);
    const stale = queue.hydrate(rowId);
    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledTimes(1));

    await expect(queue.load(true)).resolves.toBe(true);
    const current = queue.hydrate(rowId);
    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledTimes(2));
    hydrationTwo.resolve({ ...segment(), id: rowId, speakerId: 'current-authority' });
    await expect(current).resolves.not.toBeNull();
    const currentReceipt = queue.projectionReceipt();
    expect(currentReceipt).not.toBeNull();
    expect(queue.current()).toMatchObject({ id: rowId, speakerId: 'current-authority' });

    hydrationOne.resolve({ ...segment(), id: rowId, speakerId: 'retired-authority' });
    await expect(stale).resolves.toBeNull();
    expect(queue.current()).toMatchObject({ id: rowId, speakerId: 'current-authority' });
    expect(queue.projectionReceipt()).toBe(currentReceipt);

    const reconciliationReceipt = await queue.reloadProjection();
    expect(reconciliationReceipt).not.toBeNull();
    expect(queue.projectionReceipt()).toBe(reconciliationReceipt);
    expect(queue.current()).toMatchObject({ id: rowId, speakerId: 'current-authority' });
  });

  it('never publishes a disposed queue hydration after its replacement global row', async () => {
    const rowId = 'disposed-hydration';
    const lateHydration = deferred<SpeechSegment>();
    mocks.getReviewPageV1.mockResolvedValue({
      items: [
        {
          segment: { ...segment(), id: rowId, rawTranscript: 'page snapshot' },
          baseRevision: 4,
          eligible: true,
          disabledReason: null,
        },
      ],
      total: 1,
      nextCursor: null,
      scopeLabel: 'pending',
      focusNarrowed: false,
    });
    mocks.getSegment.mockReturnValue(lateHydration.promise);

    const queue = createReviewModeQueueController();
    await expect(queue.load(true)).resolves.toBe(true);
    const stale = queue.hydrate(rowId);
    await vi.waitFor(() => expect(mocks.getSegment).toHaveBeenCalledOnce());
    queue.dispose();

    const replacement = {
      ...segment(),
      id: rowId,
      rawTranscript: 'replacement authority',
      speakerId: 'replacement-surface',
    };
    segments.set([replacement]);
    lateHydration.resolve({
      ...segment(),
      id: rowId,
      rawTranscript: 'stale pre-unmount authority',
      speakerId: 'destroyed-surface',
    });

    await expect(stale).resolves.toBeNull();
    expect(get(segments)).toEqual([replacement]);
    expect(queue.current()).toBeNull();
    expect(queue.projectionReceipt()).toBeNull();
  });

  it('never publishes a re-transcription read that completes after ReviewMode unmounts', async () => {
    const full = segment();
    const lateReload = deferred<SpeechSegment>();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValueOnce(full).mockReturnValueOnce(lateReload.promise);

    const view = render(ReviewMode);
    expect(await screen.findByRole('textbox')).toHaveValue(full.rawTranscript);
    await fireEvent.click(screen.getByRole('button', { name: ckb['review.retranscribeChampion'] }));
    await waitFor(() => expect(mocks.transcribeSegment).toHaveBeenCalledOnce());
    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledTimes(2));

    view.unmount();
    const replacement = {
      ...full,
      rawTranscript: 'replacement surface authority',
      speakerId: 'replacement-surface',
    };
    segments.set([replacement]);
    lateReload.resolve({
      ...full,
      rawTranscript: 'stale re-transcription authority',
      speakerId: 'destroyed-surface',
    });
    await lateReload.promise;
    await Promise.resolve();

    expect(get(segments)).toEqual([replacement]);
  });

  it('never installs an old same-id hydration under a newer review revision', async () => {
    const stale = { ...segment(), rawTranscript: 'stale revision zero' };
    const fresh = { ...segment(), rawTranscript: 'fresh revision one' };
    let resolveStale!: (value: SpeechSegment) => void;
    let resolveFresh!: (value: SpeechSegment) => void;
    mocks.getSegmentsPage
      .mockResolvedValueOnce({
        items: [{ ...stale, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [stale.id]: 0 },
      })
      .mockResolvedValueOnce({
        items: [{ ...fresh, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [fresh.id]: 1 },
      });
    mocks.getSegment
      .mockReturnValueOnce(
        new Promise<SpeechSegment>((resolve) => {
          resolveStale = resolve;
        }),
      )
      .mockReturnValueOnce(
        new Promise<SpeechSegment>((resolve) => {
          resolveFresh = resolve;
        }),
      );

    render(ReviewMode);
    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledTimes(1));
    searchQuery.set('fresh');
    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledTimes(2));

    resolveFresh(fresh);
    expect(await screen.findByRole('textbox')).toHaveValue('fresh revision one');
    resolveStale(stale);
    await Promise.resolve();
    expect(screen.getByRole('textbox')).toHaveValue('fresh revision one');
  });

  it('unmounts the background player and retires its playback authority while the inbox is open', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await waitFor(() => expect(mocks.beginDesktopPlaybackSessionV1).toHaveBeenCalledTimes(1));
    expect(document.querySelector('audio')).not.toBeNull();

    showReviewInbox.set(true);
    await waitFor(() => expect(document.querySelector('audio')).toBeNull());
    await waitFor(() => expect(mocks.cancelDesktopPlaybackSessionV1).toHaveBeenCalledTimes(1));
  });

  it('restores a matching crash-safe draft without making it server truth', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.getReviewDraftV1.mockResolvedValue({
      segmentId: full.id,
      baseRevision: 0,
      text: 'ڕەشنووسی نەخوازراو',
      updatedAt: '2026-08-25T12:00:00.000Z',
    });

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await waitFor(() => expect(editor).toHaveValue('ڕەشنووسی نەخوازراو'));
    expect(screen.getByText(ckb['review.draftRecovered'])).toBeInTheDocument();
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();
  });

  it('shows stale local text beside server truth and never merges it automatically', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 3 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.getReviewDraftV1.mockResolvedValue({
      segmentId: full.id,
      baseRevision: 2,
      text: 'ڕەشنووسی کۆن',
      updatedAt: '2026-08-25T12:00:00.000Z',
    });

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    expect(await screen.findByText(ckb['review.draftConflictTitle'])).toBeInTheDocument();
    expect(screen.getByText('ڕەشنووسی کۆن')).toBeInTheDocument();
    expect(screen.getAllByText(full.rawTranscript).length).toBeGreaterThan(0);
    expect(editor).toHaveValue(full.rawTranscript);
    expect(mocks.saveReviewDraftV1).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: ckb['review.acceptAsIs'] })).toBeDisabled();
    expect(screen.getByRole('button', { name: ckb['review.saveNext'] })).toBeDisabled();
    expect(screen.getByRole('button', { name: ckb['review.markBad'] })).toBeDisabled();
  });

  it('allows Backspace to undo a technical flag while its prior draft remains conflicted', async () => {
    const flagged = {
      ...segment(),
      id: 'technical-flag-with-draft',
      verdict: 'escalated',
      rationale: 'private technical evidence is backend-only',
      escalated: true,
    };
    let currentRevision = 5;
    let serverRow: SpeechSegment = flagged;
    mocks.getSegmentsPage.mockImplementation(async () => ({
      items: [serverRow],
      total: 1,
      nextCursor: null,
      revisions: { [flagged.id]: currentRevision },
    }));
    mocks.getSegment.mockImplementation(async () => serverRow);
    mocks.getReviewDraftV1.mockResolvedValue({
      segmentId: flagged.id,
      baseRevision: 4,
      text: 'ڕەشنووسی پارێزراوی پێش نیشانکردن',
      updatedAt: '2026-08-28T12:00:00.000Z',
    });
    const target = technicalFlagUndoTarget(
      flagged.id,
      '77777777-7777-4777-8777-777777777777',
      4,
      'corruptContainer',
      202,
    );
    undoAvailability = { status: 'available', target };
    mocks.undoDesktopReviewActionV1.mockImplementationOnce(async (received: UndoTarget) => {
      currentRevision = 6;
      serverRow = { ...flagged, verdict: null, rationale: null, escalated: false };
      undoAvailability = { status: 'blocked', reason: 'latestFlagUndone' };
      return {
        status: 'applied',
        effectKind: 'flag',
        effectEventId: received.effectEventId,
        restoredRevision: currentRevision,
        segment: serverRow,
      };
    });

    render(ReviewMode);
    expect(await screen.findByText(ckb['review.draftConflictTitle'])).toBeInTheDocument();
    const undo = screen.getByRole('button', { name: ckb['review.undoLast'] });
    expect(undo).toBeEnabled();
    await fireEvent.keyDown(window, { key: 'Backspace', code: 'Backspace' });

    await waitFor(() => expect(mocks.undoDesktopReviewActionV1).toHaveBeenCalledOnce());
    expect(mocks.undoDesktopReviewActionV1).toHaveBeenCalledWith(
      target,
      expect.stringMatching(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
      ),
    );
    await waitFor(() =>
      expect(sharedDurableReviewUndo.state).toMatchObject({
        status: 'blocked',
        blockedReason: 'latestFlagUndone',
      }),
    );
    expect(mocks.undoDesktopReviewActionV1).toHaveBeenCalledOnce();
    expect(mocks.deleteReviewDraftV1).not.toHaveBeenCalled();
    expect(mocks.saveReviewDraftV1).not.toHaveBeenCalled();
    expect(screen.getByText('ڕەشنووسی پارێزراوی پێش نیشانکردن')).toBeInTheDocument();
  });

  it('discards a stale draft only after a global confirmation for the exact revision', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 3 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.getReviewDraftV1.mockResolvedValue({
      segmentId: full.id,
      baseRevision: 2,
      text: 'ڕەشنووسی کۆن',
      updatedAt: '2026-08-25T12:00:00.000Z',
    });

    render(ReviewMode);
    expect(await screen.findByText(ckb['review.draftConflictTitle'])).toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: ckb['review.discardLocalDraft'] }));

    const cancelledConfirmation = get(showConfirmDialog);
    expect(cancelledConfirmation).toMatchObject({
      title: ckb['review.discardDraftConfirmTitle'],
      message: ckb['review.discardDraftConfirmMessage'],
      confirmLabel: ckb['review.discardLocalDraft'],
      danger: true,
    });
    expect(mocks.deleteReviewDraftV1).not.toHaveBeenCalled();
    showConfirmDialog.set(null);
    cancelledConfirmation?.onCancel?.();
    expect(mocks.deleteReviewDraftV1).not.toHaveBeenCalled();
    expect(screen.getByText(ckb['review.draftConflictTitle'])).toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: ckb['review.discardLocalDraft'] }));
    const confirmed = get(showConfirmDialog);
    expect(confirmed).not.toBeNull();
    showConfirmDialog.set(null);
    await confirmed?.onConfirm();

    await waitFor(() => expect(mocks.deleteReviewDraftV1).toHaveBeenCalledWith(full.id, 2));
    expect(screen.queryByText(ckb['review.draftConflictTitle'])).not.toBeInTheDocument();
    expect(screen.getByRole('textbox')).toHaveValue(full.rawTranscript);
    expect(screen.getByRole('button', { name: ckb['review.acceptAsIs'] })).not.toBeDisabled();
  });

  it('refuses a stale draft confirmation after the selected review authority changes', async () => {
    const first = { ...segment(), rawTranscript: 'first authoritative transcript' };
    const second = {
      ...segment(),
      id: 'review-2',
      audioPath: 'C:\\audio\\review-2.wav',
      rawTranscript: 'second authoritative transcript',
    };
    mocks.getSegmentsPage.mockImplementation(async ({ query }: { query?: string | null }) => {
      const selected = query === 'second' ? second : first;
      return {
        items: [{ ...selected, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [selected.id]: selected.id === first.id ? 3 : 9 },
      };
    });
    mocks.getSegment.mockImplementation(async (id: string) => (id === first.id ? first : second));
    mocks.getReviewDraftV1.mockImplementation(async (id: string) =>
      id === first.id
        ? {
            segmentId: first.id,
            baseRevision: 2,
            text: 'stale local correction',
            updatedAt: '2026-08-25T12:00:00.000Z',
          }
        : null,
    );

    render(ReviewMode);
    expect(await screen.findByText(ckb['review.draftConflictTitle'])).toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: ckb['review.discardLocalDraft'] }));
    const staleConfirmation = get(showConfirmDialog);
    expect(staleConfirmation).not.toBeNull();

    searchQuery.set('second');
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue(second.rawTranscript));
    showConfirmDialog.set(null);
    await staleConfirmation?.onConfirm();

    expect(mocks.deleteReviewDraftV1).not.toHaveBeenCalled();
    expect(screen.getByRole('textbox')).toHaveValue(second.rawTranscript);
  });

  it('blocks every truth action and shortcut until draft recovery has succeeded', async () => {
    const full = segment();
    let resolveDraft!: (value: null) => void;
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.getReviewDraftV1.mockReturnValue(
      new Promise<null>((resolve) => {
        resolveDraft = resolve;
      }),
    );

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(full.id));
    await hearCurrentAudio();

    const accept = screen.getByRole('button', { name: ckb['review.acceptAsIs'] });
    const save = screen.getByRole('button', { name: ckb['review.saveNext'] });
    const reject = screen.getByRole('button', { name: ckb['review.markBad'] });
    expect(accept).toBeDisabled();
    expect(save).toBeDisabled();
    expect(reject).toBeDisabled();
    expect(accept).toHaveAttribute('aria-describedby', 'review-draft-disabled-reason');
    expect(reject).toHaveAttribute('aria-describedby', 'review-draft-disabled-reason');

    await fireEvent.keyDown(window, { key: 'a', code: 'KeyA' });
    await fireEvent.keyDown(window, { key: 'x', code: 'KeyX' });
    await fireEvent.keyDown(window, { key: 'Enter', code: 'Enter', ctrlKey: true });
    expect(mocks.recordPlaybackReceipt).not.toHaveBeenCalled();
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();
    expect(mocks.markSegmentUnusableV1).not.toHaveBeenCalled();

    resolveDraft(null);
    await waitFor(() => expect(accept).not.toBeDisabled());
    expect(save).not.toBeDisabled();
    expect(reject).not.toBeDisabled();
  });

  it('refuses a wrong-segment draft response and recovers only after an exact retry', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.getReviewDraftV1
      .mockResolvedValueOnce({
        segmentId: 'another-segment',
        baseRevision: 0,
        text: 'دەقی کلیپێکی تر',
        updatedAt: '2026-08-25T12:00:00.000Z',
      })
      .mockResolvedValueOnce(null);

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    expect(await screen.findByText(ckb['review.draftLoadFailedHint'])).toBeInTheDocument();
    expect(editor).toHaveValue(full.rawTranscript);
    expect(editor).not.toHaveValue('دەقی کلیپێکی تر');
    const accept = screen.getByRole('button', { name: ckb['review.acceptAsIs'] });
    expect(accept).toBeDisabled();

    await fireEvent.click(screen.getByRole('button', { name: ckb.retry }));
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(accept).not.toBeDisabled());
    expect(editor).toHaveValue(full.rawTranscript);
  });

  it('never deletes an unseen draft when recovery fails during close or a scope switch', async () => {
    const first = { ...segment(), rawTranscript: 'first server transcript' };
    const second = { ...segment(), id: 'review-2', rawTranscript: 'second server transcript' };
    mocks.getSegmentsPage
      .mockResolvedValueOnce({
        items: [{ ...first, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [first.id]: 4 },
      })
      .mockResolvedValueOnce({
        items: [{ ...second, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [second.id]: 9 },
      });
    mocks.getSegment.mockImplementation(async (id: string) => (id === first.id ? first : second));
    mocks.getReviewDraftV1
      .mockRejectedValueOnce(new Error('draft database unavailable'))
      .mockResolvedValueOnce(null);

    render(ReviewMode);
    expect(await screen.findByText(ckb['review.draftLoadFailedHint'])).toBeInTheDocument();
    await expect(flushReviewDrafts()).rejects.toThrow();
    expect(mocks.deleteReviewDraftV1).not.toHaveBeenCalled();
    expect(mocks.saveReviewDraftV1).not.toHaveBeenCalled();

    searchQuery.set('second');
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue(second.rawTranscript));
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(second.id));
    expect(mocks.deleteReviewDraftV1).not.toHaveBeenCalled();
    expect(mocks.saveReviewDraftV1).not.toHaveBeenCalled();
  });

  it('serializes a fast navigation round-trip behind the outgoing draft write', async () => {
    const first = { ...segment(), rawTranscript: 'first transcript' };
    const second = { ...segment(), id: 'review-2', rawTranscript: 'second transcript' };
    let releaseSave!: () => void;
    const saveBarrier = new Promise<void>((resolve) => {
      releaseSave = resolve;
    });
    let durableFirstDraft: string | null = null;
    mocks.getSegmentsPage.mockResolvedValue({
      items: [
        { ...first, alignmentJson: null, evidenceJson: null },
        { ...second, alignmentJson: null, evidenceJson: null },
      ],
      total: 2,
      nextCursor: null,
      revisions: { [first.id]: 4, [second.id]: 9 },
    });
    mocks.getSegment.mockImplementation(async (id: string) => (id === first.id ? first : second));
    mocks.getReviewDraftV1.mockImplementation(async (id: string) =>
      id === first.id && durableFirstDraft !== null
        ? {
            segmentId: first.id,
            baseRevision: 4,
            text: durableFirstDraft,
            updatedAt: '2026-08-25T12:00:00.000Z',
          }
        : null,
    );
    mocks.saveReviewDraftV1.mockImplementation(async (segmentId, baseRevision, text) => {
      if (segmentId === first.id) {
        await saveBarrier;
        durableFirstDraft = text;
      }
      return { segmentId, baseRevision, text, updatedAt: '2026-08-25T12:00:00.000Z' };
    });

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(first.id));
    await fireEvent.input(editor, { target: { value: 'latest unsaved human text' } });
    await fireEvent.blur(editor);
    await fireEvent.keyDown(window, { key: 'n', code: 'KeyN' });
    await waitFor(() => expect(editor).toHaveValue(second.rawTranscript));
    await waitFor(() =>
      expect(mocks.saveReviewDraftV1).toHaveBeenCalledWith(
        first.id,
        4,
        'latest unsaved human text',
      ),
    );

    await fireEvent.keyDown(window, { key: 'p', code: 'KeyP' });
    await waitFor(() => expect(editor).toHaveValue('latest unsaved human text'));
    expect(mocks.getReviewDraftV1.mock.calls.filter(([id]) => id === first.id)).toHaveLength(1);

    releaseSave();
    await waitFor(() =>
      expect(mocks.getReviewDraftV1.mock.calls.filter(([id]) => id === first.id)).toHaveLength(2),
    );
    expect(editor).toHaveValue('latest unsaved human text');
  });

  it('retries a settled off-screen draft failure at close after A to B navigation', async () => {
    const first = { ...segment(), id: 'review-a', rawTranscript: 'first transcript' };
    const second = { ...segment(), id: 'review-b', rawTranscript: 'second transcript' };
    mocks.getSegmentsPage.mockResolvedValue({
      items: [
        { ...first, alignmentJson: null, evidenceJson: null },
        { ...second, alignmentJson: null, evidenceJson: null },
      ],
      total: 2,
      nextCursor: null,
      revisions: { [first.id]: 4, [second.id]: 9 },
    });
    mocks.getSegment.mockImplementation(async (id: string) => (id === first.id ? first : second));
    mocks.saveReviewDraftV1
      .mockRejectedValueOnce(new Error('first write failed'))
      .mockImplementation(async (segmentId, baseRevision, text) => ({
        segmentId,
        baseRevision,
        text,
        updatedAt: '2026-08-25T12:00:00.000Z',
      }));

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(first.id));
    await fireEvent.input(editor, { target: { value: 'exact off-screen A' } });
    await fireEvent.keyDown(window, { key: 'n', code: 'KeyN' });
    await waitFor(() => expect(editor).toHaveValue(second.rawTranscript));
    await waitFor(() =>
      expect(mocks.saveReviewDraftV1).toHaveBeenCalledWith(first.id, 4, 'exact off-screen A'),
    );
    expect(mocks.saveReviewDraftV1).toHaveBeenCalledTimes(1);

    // The first Promise has already rejected and disappeared from the old in-flight map shape. The
    // desired-state coordinator must still retain A and retry it through the global close barrier.
    await Promise.resolve();
    await flushReviewDrafts();

    expect(mocks.saveReviewDraftV1).toHaveBeenCalledTimes(2);
    expect(mocks.saveReviewDraftV1).toHaveBeenNthCalledWith(2, first.id, 4, 'exact off-screen A');
  });

  it('debounces an edited transcript into the revision-bound draft command', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 7 },
    });
    mocks.getSegment.mockResolvedValue(full);

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(full.id));
    await fireEvent.input(editor, { target: { value: 'دەقی ڕاستکراوە' } });
    await waitFor(
      () => expect(mocks.saveReviewDraftV1).toHaveBeenCalledWith(full.id, 7, 'دەقی ڕاستکراوە'),
      { timeout: 1_500 },
    );
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();
  });

  it('flushes an immediate pre-close edit without waiting for the debounce timer', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 11 },
    });
    mocks.getSegment.mockResolvedValue(full);

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(full.id));
    await fireEvent.input(editor, { target: { value: 'ڕەشنووسی پێش داخستن' } });
    expect(mocks.saveReviewDraftV1).not.toHaveBeenCalled();

    await flushReviewDrafts();
    expect(mocks.saveReviewDraftV1).toHaveBeenCalledWith(full.id, 11, 'ڕەشنووسی پێش داخستن');
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();
  });

  it('durably flushes the exact visible correction before playback finalization and commit', async () => {
    const full = segment();
    const order: string[] = [];
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.saveReviewDraftV1.mockImplementation(async (segmentId, baseRevision, text) => {
      order.push('draft');
      return { segmentId, baseRevision, text, updatedAt: '2026-08-25T12:00:00.000Z' };
    });
    mocks.recordPlaybackReceipt.mockImplementation(async (request) => {
      order.push('receipt');
      return {
        playbackReceiptId: request.playbackReceiptId,
        segmentId: full.id,
        segmentRevision: 0,
        uniquePlayedMs: 900,
        clipDurationMs: 1000,
        coverageRatio: 0.9,
      };
    });
    mocks.commitReviewV1.mockImplementation(async (request) => {
      order.push('commit');
      removedReviewSegmentIds.add(request.segmentId);
      undoAvailability = {
        status: 'available',
        target: undoTarget(request.segmentId, request.decision, request.operationId, 303),
      };
      return {
        segmentId: request.segmentId,
        committedRevision: request.baseRevision + 1,
        authoritativeTranscript: request.transcript ?? full.rawTranscript,
        decisionId: 'effect:303',
      };
    });

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(full.id));
    await hearCurrentAudio();
    await fireEvent.input(editor, { target: { value: 'دەقی دەستکاری‌کراوی نوێ' } });
    await fireEvent.click(screen.getByRole('button', { name: ckb['review.saveNext'] }));

    await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledTimes(1));
    expect(order).toEqual(['draft', 'receipt', 'commit']);
    expect(mocks.saveReviewDraftV1).toHaveBeenCalledWith(full.id, 0, 'دەقی دەستکاری‌کراوی نوێ');
  });

  it('freezes every transcript mutation through the writer and projection barrier without new draft IPC', async () => {
    const full = segment();
    const submittedText = 'دەقی دەستکاری‌کراوی جێگیر';
    const commit = deferred<{
      segmentId: string;
      committedRevision: number;
      authoritativeTranscript: string;
      decisionId: string;
    }>();
    const projection = deferred<{
      items: never[];
      total: number;
      nextCursor: null;
      scopeLabel: string;
      focusNarrowed: boolean;
    }>();
    let reviewPageCall = 0;
    mocks.getReviewPageV1.mockImplementation(async () => {
      reviewPageCall += 1;
      if (reviewPageCall === 1) {
        return {
          items: [
            {
              segment: { ...full, alignmentJson: null, evidenceJson: null },
              baseRevision: 0,
              eligible: true,
              disabledReason: null,
            },
          ],
          total: 1,
          nextCursor: null,
          scopeLabel: 'pending',
          focusNarrowed: false,
        };
      }
      return projection.promise;
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.commitReviewV1.mockImplementation(async (request) => {
      undoAvailability = {
        status: 'available',
        target: undoTarget(request.segmentId, request.decision, request.operationId, 707),
      };
      return commit.promise;
    });

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(full.id));
    await hearCurrentAudio();
    await fireEvent.input(editor, { target: { value: submittedText } });
    await fireEvent.click(screen.getByRole('button', { name: ckb['review.saveNext'] }));
    await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledTimes(1));

    const draftWritesAtCommit = mocks.saveReviewDraftV1.mock.calls.length;
    const reset = screen.getByRole('button', { name: ckb['review.reset'] });
    const wordChip = document.querySelector<HTMLButtonElement>('[data-chip="0"]');
    expect(wordChip).not.toBeNull();

    const assertMutationBarrier = async (attemptedText: string) => {
      expect(editor).toBeDisabled();
      expect(reset).toBeDisabled();
      expect(wordChip).toBeDisabled();
      await fireEvent.input(editor, { target: { value: attemptedText } });
      await fireEvent.dblClick(wordChip!);
      await fireEvent.click(reset);
      await flushReviewDrafts();
      expect(screen.queryByRole('textbox', { name: ckb['review.editWordAria'] })).toBeNull();
      expect(get(showConfirmDialog)).toBeNull();
      expect(mocks.saveReviewDraftV1).toHaveBeenCalledTimes(draftWritesAtCommit);
      expect(mocks.deleteReviewDraftV1).not.toHaveBeenCalled();
    };

    await assertMutationBarrier('writer-stage-clobber');

    commit.resolve({
      segmentId: full.id,
      committedRevision: 1,
      authoritativeTranscript: submittedText,
      decisionId: 'effect:707',
    });
    await waitFor(() => expect(reviewPageCall).toBe(2));
    await waitFor(() => expect(sharedDurableReviewUndo.state.truthProjectionPending).toBe(true));

    await assertMutationBarrier('projection-stage-clobber');

    projection.resolve({
      items: [],
      total: 0,
      nextCursor: null,
      scopeLabel: 'pending',
      focusNarrowed: false,
    });
    await waitFor(() => expect(screen.queryByRole('textbox')).not.toBeInTheDocument());
    expect(mocks.commitReviewV1.mock.calls[0][0]).toMatchObject({
      segmentId: full.id,
      transcript: submittedText,
      decision: 'edit',
    });
  });

  it('invalidates a pre-open word editor when a truth lease begins', async () => {
    const full = segment();
    const submittedText = 'دەقی جێگیر بۆ ناردن';
    const commit = deferred<{
      segmentId: string;
      committedRevision: number;
      authoritativeTranscript: string;
      decisionId: string;
    }>();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.commitReviewV1.mockImplementation(async (request) => {
      removedReviewSegmentIds.add(request.segmentId);
      undoAvailability = {
        status: 'available',
        target: undoTarget(request.segmentId, request.decision, request.operationId, 708),
      };
      return commit.promise;
    });

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(full.id));
    await hearCurrentAudio();
    await fireEvent.input(editor, { target: { value: submittedText } });

    const wordChip = document.querySelector<HTMLButtonElement>('[data-chip="0"]');
    expect(wordChip).not.toBeNull();
    await fireEvent.dblClick(wordChip!);
    const wordInput = await screen.findByRole('textbox', { name: ckb['review.editWordAria'] });

    await fireEvent.click(screen.getByRole('button', { name: ckb['review.saveNext'] }));
    await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledTimes(1));
    const draftWritesAtCommit = mocks.saveReviewDraftV1.mock.calls.length;

    expect(editor).toBeDisabled();
    await waitFor(() => expect(wordInput).not.toBeInTheDocument());
    await fireEvent.input(wordInput, { target: { value: 'دەقی گۆڕدراوی درەنگ' } });
    await fireEvent.keyDown(wordInput, { key: 'Enter', code: 'Enter' });
    await flushReviewDrafts();

    expect(mocks.saveReviewDraftV1).toHaveBeenCalledTimes(draftWritesAtCommit);
    expect(mocks.deleteReviewDraftV1).not.toHaveBeenCalled();

    commit.resolve({
      segmentId: full.id,
      committedRevision: 1,
      authoritativeTranscript: submittedText,
      decisionId: 'effect:708',
    });
    await waitFor(() => expect(screen.queryByRole('textbox')).not.toBeInTheDocument());
    expect(mocks.commitReviewV1.mock.calls[0][0]).toMatchObject({ transcript: submittedText });
  });

  it('invalidates an exact reset confirmation captured before the truth lease', async () => {
    const full = segment();
    const submittedText = 'دەقی کاتی ناردن';
    const commit = deferred<{
      segmentId: string;
      committedRevision: number;
      authoritativeTranscript: string;
      decisionId: string;
    }>();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.commitReviewV1.mockImplementation(async (request) => {
      removedReviewSegmentIds.add(request.segmentId);
      undoAvailability = {
        status: 'available',
        target: undoTarget(request.segmentId, request.decision, request.operationId, 709),
      };
      return commit.promise;
    });

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(full.id));
    await hearCurrentAudio();
    await fireEvent.input(editor, { target: { value: submittedText } });

    const reset = screen.getByRole('button', { name: ckb['review.reset'] });
    await fireEvent.click(reset);
    const preLeaseReset = get(showConfirmDialog);
    expect(preLeaseReset).not.toBeNull();

    await fireEvent.click(screen.getByRole('button', { name: ckb['review.saveNext'] }));
    await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledTimes(1));
    const draftWritesAtCommit = mocks.saveReviewDraftV1.mock.calls.length;
    expect(reset).toBeDisabled();

    await preLeaseReset?.onConfirm();
    await flushReviewDrafts();
    expect(mocks.saveReviewDraftV1).toHaveBeenCalledTimes(draftWritesAtCommit);
    expect(mocks.deleteReviewDraftV1).not.toHaveBeenCalled();

    commit.resolve({
      segmentId: full.id,
      committedRevision: 1,
      authoritativeTranscript: submittedText,
      decisionId: 'effect:709',
    });
    await waitFor(() => expect(screen.queryByRole('textbox')).not.toBeInTheDocument());
    expect(mocks.commitReviewV1.mock.calls[0][0]).toMatchObject({ transcript: submittedText });
  });

  it('cannot discard a typed correction through Accept or the A shortcut', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 7 },
    });
    mocks.getSegment.mockResolvedValue(full);

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'دەقی ڕاستکراوە' } });
    const accept = screen.getByRole('button', {
      name: new RegExp(ckb['review.acceptAsIs']),
    });
    expect(accept).toBeDisabled();
    expect(accept).toHaveAttribute('aria-describedby', 'review-accept-disabled-reason');
    const reject = screen.getByRole('button', { name: ckb['review.markBad'] });
    expect(reject).toBeDisabled();
    expect(reject).toHaveAttribute('aria-describedby', 'review-reject-disabled-reason');

    await fireEvent.blur(editor);
    await fireEvent.keyDown(window, { key: 'a', code: 'KeyA' });
    await fireEvent.keyDown(window, { key: 'x', code: 'KeyX' });
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();
    expect(editor).toHaveValue('دەقی ڕاستکراوە');
  });

  it('preserves server review eligibility through hydration and blocks every truth action', async () => {
    const full = segment();
    mocks.getReviewPageV1.mockResolvedValueOnce({
      items: [
        {
          segment: { ...full, alignmentJson: null, evidenceJson: null },
          baseRevision: 7,
          eligible: false,
          disabledReason: 'TRANSCRIPT_NOT_READY',
        },
      ],
      total: 1,
      nextCursor: null,
      scopeLabel: 'pending',
      focusNarrowed: false,
    });
    mocks.getSegment.mockResolvedValue(full);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    const reason = document.getElementById('review-eligibility-disabled-reason');
    expect(reason).toHaveTextContent(ckb['review.transcriptNotReady']);

    const accept = screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) });
    const save = screen.getByRole('button', { name: ckb['review.saveNext'] });
    const reject = screen.getByRole('button', { name: ckb['review.markBad'] });
    for (const action of [accept, save, reject]) {
      expect(action).toBeDisabled();
      expect(action).toHaveAttribute('aria-describedby', 'review-eligibility-disabled-reason');
    }

    await fireEvent.keyDown(window, { key: 'a', code: 'KeyA' });
    await fireEvent.keyDown(window, { key: 'x', code: 'KeyX' });
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();
  });

  it('keeps the next row non-actionable after a decision until that row is fully hydrated', async () => {
    const first = segment();
    const second: SpeechSegment = {
      ...segment(),
      id: 'review-2',
      audioPath: 'C:\\audio\\review-2.wav',
      alignmentJson: JSON.stringify({
        source_start_ms: 8_000,
        source_end_ms: 9_000,
        chunk_index: 1,
        chunk_count: 3,
        words: [{ word: 'دەقی', start: 0, end: 0.4, confidence: 0.9 }],
      }),
    };
    let resolveSecond!: (value: SpeechSegment) => void;
    mocks.getDatasetStats.mockResolvedValue({ totalSegments: 2, verifiedCount: 0 });
    mocks.getSegmentsPage.mockResolvedValue({
      items: [
        { ...first, alignmentJson: null, evidenceJson: null },
        { ...second, alignmentJson: null, evidenceJson: null },
      ],
      total: 2,
      nextCursor: null,
    });
    mocks.getSegment.mockResolvedValueOnce(first).mockReturnValueOnce(
      new Promise<SpeechSegment>((resolve) => {
        resolveSecond = resolve;
      }),
    );

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await hearCurrentAudio();
    await fireEvent.click(
      screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) }),
    );

    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledWith('review-2'));
    expect(screen.queryByTestId('review-action-bar')).not.toBeInTheDocument();
    expect(mocks.alignSegment).not.toHaveBeenCalled();

    resolveSecond(second);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(mocks.alignSegment).not.toHaveBeenCalled();
  });

  it('accrues the final between-timeupdate delta before freezing an exact-threshold receipt', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    const audio = document.querySelector('audio')!;
    await waitFor(() => expect(audio.getAttribute('src')).toBeTruthy());
    Object.defineProperty(audio, 'duration', { configurable: true, value: 1 });
    audio.currentTime = 0;
    await fireEvent.loadedMetadata(audio);
    const play = await waitFor(() => {
      const button = document.querySelector<HTMLButtonElement>(
        '[data-testid="audio-player-timeline"] button',
      );
      expect(button).not.toBeNull();
      return button!;
    });
    await fireEvent.click(play);
    await waitFor(() => expect(audio.paused).toBe(false));
    audio.currentTime = 0.84;
    await fireEvent.timeUpdate(audio);
    // No timeupdate arrives for the final 10ms before the reviewer clicks. The child-owned pause
    // snapshot must accrue it synchronously; parent bindings alone still contain only 840ms here.
    audio.currentTime = 0.85;

    await fireEvent.click(
      screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) }),
    );

    await waitFor(() => expect(mocks.recordPlaybackReceipt).toHaveBeenCalledTimes(1));
    expect(mocks.recordPlaybackReceipt).toHaveBeenCalledWith({
      playbackReceiptId: '11111111-1111-4111-8111-111111111111',
      mediaGrantId: MEDIA_GRANT_ID,
      intervals: [{ startMs: 0, endMs: 850 }],
    });
    expect(mocks.commitReviewV1).toHaveBeenCalledTimes(1);
  });

  it('uses the server session duration and refuses a local-span threshold one millisecond short', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.beginDesktopPlaybackSessionV1.mockImplementation(
      async (segmentId: string, _grant: string, expectedRevision: number) => ({
        playbackReceiptId: '11111111-1111-4111-8111-111111111111',
        segmentId,
        segmentRevision: expectedRevision,
        clipDurationMs: 1_001,
        expiresAtMs: Date.now() + 60_000,
      }),
    );

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    const audio = document.querySelector('audio')!;
    await waitFor(() => expect(audio.getAttribute('src')).toBeTruthy());
    Object.defineProperty(audio, 'duration', { configurable: true, value: 1 });
    audio.currentTime = 0;
    await fireEvent.loadedMetadata(audio);
    const play = await waitFor(() => {
      const button = document.querySelector<HTMLButtonElement>(
        '[data-testid="audio-player-timeline"] button',
      );
      expect(button).not.toBeNull();
      return button!;
    });
    await fireEvent.click(play);
    await waitFor(() => expect(audio.paused).toBe(false));
    audio.currentTime = 0.85;

    await fireEvent.click(
      screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) }),
    );
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(mocks.recordPlaybackReceipt).not.toHaveBeenCalled();
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();
  });

  it('reissues authority for a stale row revision and never carries the old receipt forward', async () => {
    const full = segment();
    mocks.getSegmentsPage
      .mockResolvedValueOnce({
        items: [{ ...full, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [full.id]: 7 },
      })
      .mockResolvedValueOnce({
        items: [{ ...full, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [full.id]: 8 },
      });
    mocks.getSegment.mockResolvedValue(full);
    mocks.beginDesktopPlaybackSessionV1.mockImplementation(
      async (segmentId: string, _grant: string, expectedRevision: number) => ({
        playbackReceiptId: `receipt-revision-${expectedRevision}`,
        segmentId,
        segmentRevision: expectedRevision,
        clipDurationMs: 1_000,
        expiresAtMs: Date.now() + 60_000,
      }),
    );
    mocks.recordPlaybackReceipt.mockImplementation(async ({ playbackReceiptId }) => ({
      playbackReceiptId,
      segmentId: full.id,
      segmentRevision: Number(playbackReceiptId.replace('receipt-revision-', '')),
      uniquePlayedMs: 900,
      clipDurationMs: 1_000,
      coverageRatio: 0.9,
    }));
    mocks.commitReviewV1
      .mockRejectedValueOnce({
        schema: 1,
        code: 'STALE_REVISION',
        message: 'row changed',
        retryable: false,
      })
      .mockImplementationOnce(async (request) => ({
        segmentId: request.segmentId,
        committedRevision: request.baseRevision + 1,
        authoritativeTranscript: request.transcript ?? full.rawTranscript,
        decisionId: 'effect:808',
      }));

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await waitFor(() =>
      expect(mocks.beginDesktopPlaybackSessionV1).toHaveBeenCalledWith(
        full.id,
        MEDIA_GRANT_ID,
        7,
        expect.any(String),
      ),
    );
    await hearCurrentAudio();
    await fireEvent.click(
      screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) }),
    );

    await waitFor(() =>
      expect(mocks.beginDesktopPlaybackSessionV1).toHaveBeenCalledWith(
        full.id,
        MEDIA_GRANT_ID,
        8,
        expect.any(String),
      ),
    );
    const issuanceCalls = mocks.beginDesktopPlaybackSessionV1.mock.calls;
    expect(issuanceCalls[1][3]).not.toBe(issuanceCalls[0][3]);
    await hearCurrentAudio();
    await fireEvent.click(
      screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) }),
    );

    await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledTimes(2));
    expect(mocks.recordPlaybackReceipt.mock.calls[1][0]).toMatchObject({
      playbackReceiptId: 'receipt-revision-8',
    });
    expect(mocks.commitReviewV1.mock.calls[1][0]).toMatchObject({
      baseRevision: 8,
      playbackReceiptId: 'receipt-revision-8',
    });
  });

  it('retries every ambiguous typed finalization error with the first immutable interval union', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.recordPlaybackReceipt
      .mockRejectedValueOnce({
        schema: 1,
        code: 'EVIDENCE_WRITE_FAILED',
        message: 'response stage failed',
        retryable: true,
      })
      .mockImplementationOnce(async (request) => ({
        playbackReceiptId: request.playbackReceiptId,
        segmentId: full.id,
        segmentRevision: 0,
        uniquePlayedMs: 900,
        clipDurationMs: 1_000,
        coverageRatio: 0.9,
      }));

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await hearCurrentAudio();
    const accept = screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) });
    await fireEvent.click(accept);
    await waitFor(() => expect(mocks.recordPlaybackReceipt).toHaveBeenCalledTimes(1));
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();

    const audio = document.querySelector('audio')!;
    const play = document.querySelector<HTMLButtonElement>(
      '[data-testid="audio-player-timeline"] button',
    )!;
    await fireEvent.click(play);
    await waitFor(() => expect(audio.paused).toBe(false));
    audio.currentTime = 1;
    await fireEvent.timeUpdate(audio);
    await fireEvent.click(accept);

    await waitFor(() => expect(mocks.recordPlaybackReceipt).toHaveBeenCalledTimes(2));
    expect(mocks.recordPlaybackReceipt.mock.calls[1][0]).toEqual(
      mocks.recordPlaybackReceipt.mock.calls[0][0],
    );
    expect(mocks.recordPlaybackReceipt.mock.calls[1][0].intervals).toEqual([
      { startMs: 0, endMs: 900 },
    ]);
    await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledTimes(1));
  });

  it('refuses a mismatched playback-finalization response and retries the immutable evidence', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.recordPlaybackReceipt.mockResolvedValueOnce({
      playbackReceiptId: 'wrong-receipt',
      segmentId: 'wrong-segment',
      segmentRevision: 99,
      uniquePlayedMs: 900,
      clipDurationMs: 1_000,
      coverageRatio: 0.9,
    });

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await hearCurrentAudio();
    const accept = screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) });
    await fireEvent.click(accept);
    await waitFor(() => expect(mocks.recordPlaybackReceipt).toHaveBeenCalledTimes(1));
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();
    await waitFor(() => expect(accept).not.toBeDisabled());

    await fireEvent.click(accept);
    await waitFor(() => expect(mocks.recordPlaybackReceipt).toHaveBeenCalledTimes(2));
    expect(mocks.recordPlaybackReceipt.mock.calls[1][0]).toEqual(
      mocks.recordPlaybackReceipt.mock.calls[0][0],
    );
    await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledTimes(1));
  });

  it('keeps exact review state and scope through a held then ambiguous decision response', async () => {
    const full = segment();
    const second = {
      ...segment(),
      id: 'review-2',
      audioPath: 'C:\\audio\\review-2.wav',
      rawTranscript: 'second transcript',
    };
    const commit = deferred<{
      segmentId: string;
      committedRevision: number;
      authoritativeTranscript: string;
      decisionId: string;
    }>();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [
        { ...full, alignmentJson: null, evidenceJson: null },
        { ...second, alignmentJson: null, evidenceJson: null },
      ],
      total: 2,
      nextCursor: null,
      revisions: { [full.id]: 0, [second.id]: 0 },
    });
    mocks.getSegment.mockImplementation(async (id: string) => (id === full.id ? full : second));
    mocks.commitReviewV1.mockReturnValue(commit.promise);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await hearCurrentAudio();
    const editor = screen.getByRole('textbox');
    const exactText = 'دەقی دەستکاری‌کراوی پارێزراو';
    await fireEvent.input(editor, { target: { value: exactText } });
    editor.focus();
    await fireEvent.keyDown(editor, { key: 'Enter', code: 'Enter', ctrlKey: true });
    await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledTimes(1));
    const first = mocks.commitReviewV1.mock.calls[0][0];
    const suspectToggle = screen.getByTestId('suspect-first-toggle');
    const audio = document.querySelector('audio')!;

    const assertScopeBarrier = async (scopeAttempt: string) => {
      const pageCalls = mocks.getReviewPageV1.mock.calls.length;
      const legacyPageCalls = mocks.getSegmentsPage.mock.calls.length;
      const exactFocus = document.activeElement;
      const exactSrc = audio.getAttribute('src');
      const exactTime = audio.currentTime;
      const exactPaused = audio.paused;
      const authorityCalls = mocks.beginDesktopPlaybackSessionV1.mock.calls.length;
      const play = screen.getByRole('button', { name: ckb['audio.play'] });
      const replayButton = screen.getByRole('button', { name: ckb['review.replay'] });
      const seek = screen.getByRole('slider', { name: ckb['audio.seek'] });
      const waveform = screen.getByRole('slider', { name: ckb['waveform.audioTimeline'] });
      expect(suspectToggle).toBeDisabled();
      expect(suspectToggle).toHaveAttribute('aria-describedby', 'review-scope-disabled-reason');
      expect(play).toBeDisabled();
      expect(replayButton).toBeDisabled();
      expect(seek).toBeDisabled();
      expect(waveform).toHaveAttribute('aria-disabled', 'true');
      searchQuery.set(scopeAttempt);
      await fireEvent.click(suspectToggle);
      await fireEvent.click(play);
      await fireEvent.click(replayButton);
      await fireEvent.keyDown(window, { key: ' ', code: 'Space' });
      await fireEvent.keyDown(window, { key: 'r', code: 'KeyR' });
      await fireEvent.keyDown(window, { key: 'ArrowRight', code: 'ArrowRight' });
      await fireEvent.keyDown(waveform, { key: 'ArrowRight', code: 'ArrowRight' });
      await fireEvent.pointerDown(waveform, { clientX: 300, pointerId: 1 });
      await Promise.resolve();
      expect(mocks.getReviewPageV1).toHaveBeenCalledTimes(pageCalls);
      expect(mocks.getSegmentsPage).toHaveBeenCalledTimes(legacyPageCalls);
      expect(suspectToggle).toHaveAttribute('aria-pressed', 'false');
      expect(screen.getByRole('textbox')).toHaveValue(exactText);
      expect(screen.getByTestId('review-source-file')).toHaveTextContent('review.wav');
      expect(audio.getAttribute('src')).toBe(exactSrc);
      expect(audio.currentTime).toBe(exactTime);
      expect(audio.paused).toBe(exactPaused);
      expect(mocks.beginDesktopPlaybackSessionV1).toHaveBeenCalledTimes(authorityCalls);
      expect(document.activeElement).toBe(exactFocus);
    };

    await assertScopeBarrier('held writer scope');
    commit.reject(new Error('response lost after durable commit'));
    await waitFor(() => expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(true));
    await assertScopeBarrier('ambiguous writer scope');

    const accept = screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) });
    expect(accept).toBeDisabled();
    await fireEvent.click(accept);
    expect(mocks.commitReviewV1).toHaveBeenCalledTimes(1);
    expect(first).toMatchObject({
      operationId: expect.any(String),
      playbackReceiptId: '11111111-1111-4111-8111-111111111111',
      segmentId: full.id,
      transcript: exactText,
    });
    expect(mocks.recordPlaybackReceipt).toHaveBeenCalledOnce();
  });

  it('hard-stops a wrong-segment commit response with zero speculative queue reloads', async () => {
    const full = segment();
    const response = deferred<{
      segmentId: string;
      committedRevision: number;
      authoritativeTranscript: string;
      decisionId: string;
    }>();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.commitReviewV1.mockReturnValue(response.promise);

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await hearCurrentAudio();
    const exactText = 'exact correction retained after malformed success';
    await fireEvent.input(editor, { target: { value: exactText } });
    editor.focus();
    await fireEvent.keyDown(editor, { key: 'Enter', code: 'Enter', ctrlKey: true });
    await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledOnce());

    const pageCalls = mocks.getReviewPageV1.mock.calls.length;
    const legacyPageCalls = mocks.getSegmentsPage.mock.calls.length;
    const exactFocus = document.activeElement;
    const audio = document.querySelector('audio')!;
    const exactAudio = {
      src: audio.getAttribute('src'),
      time: audio.currentTime,
      paused: audio.paused,
    };
    response.resolve({
      segmentId: 'wrong-segment',
      committedRevision: 1,
      authoritativeTranscript: 'wrong-segment transcript',
      decisionId: 'effect:999',
    });

    await waitFor(() => expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(true));
    expect(mocks.getReviewPageV1).toHaveBeenCalledTimes(pageCalls);
    expect(mocks.getSegmentsPage).toHaveBeenCalledTimes(legacyPageCalls);
    expect(screen.getByRole('textbox')).toHaveValue(exactText);
    expect(screen.getByTestId('review-source-file')).toHaveTextContent('review.wav');
    expect(audio.getAttribute('src')).toBe(exactAudio.src);
    expect(audio.currentTime).toBe(exactAudio.time);
    expect(audio.paused).toBe(exactAudio.paused);
    expect(document.activeElement).toBe(exactFocus);
  });

  it.each(['NO_PLAYBACK_EVIDENCE', 'PLAYBACK_EVIDENCE_CHANGED'])(
    'retires commit-time %s authority and requires a fresh listen plus operation',
    async (code) => {
      const full = segment();
      mocks.getSegmentsPage.mockResolvedValue({
        items: [{ ...full, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [full.id]: 0 },
      });
      mocks.getSegment.mockResolvedValue(full);
      let issuance = 0;
      mocks.beginDesktopPlaybackSessionV1.mockImplementation(
        async (segmentId: string, _mediaGrantId: string, expectedRevision: number) => ({
          playbackReceiptId: `receipt-commit-attempt-${++issuance}`,
          segmentId,
          segmentRevision: expectedRevision,
          clipDurationMs: 1_000,
          expiresAtMs: Date.now() + 60_000,
        }),
      );
      mocks.recordPlaybackReceipt.mockImplementation(
        async (request: { playbackReceiptId: string }) => ({
          playbackReceiptId: request.playbackReceiptId,
          segmentId: full.id,
          segmentRevision: 0,
          uniquePlayedMs: 900,
          clipDurationMs: 1_000,
          coverageRatio: 0.9,
        }),
      );
      mocks.commitReviewV1.mockRejectedValueOnce({
        schema: 1,
        code,
        message: 'private backend detail',
        retryable: true,
        suggestedAction: 'reloadClip',
      });

      render(ReviewMode);
      expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
      await hearCurrentAudio();
      const accept = screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) });
      await fireEvent.click(accept);
      await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledTimes(1));
      const first = mocks.commitReviewV1.mock.calls[0][0];
      await waitFor(() => expect(mocks.beginDesktopPlaybackSessionV1).toHaveBeenCalledTimes(2));

      await hearCurrentAudio();
      await fireEvent.click(accept);
      await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledTimes(2));
      const second = mocks.commitReviewV1.mock.calls[1][0];
      expect(second.operationId).not.toBe(first.operationId);
      expect(second.playbackReceiptId).not.toBe(first.playbackReceiptId);
      expect(mocks.recordPlaybackReceipt).toHaveBeenCalledTimes(2);
    },
  );

  it.each(['PLAYBACK_EVIDENCE_CHANGED'])(
    'retires a proven non-commit %s and succeeds only after a fresh grant/session listen',
    async (code) => {
      const full = segment();
      mocks.getSegmentsPage.mockResolvedValue({
        items: [{ ...full, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [full.id]: 0 },
      });
      mocks.getSegment.mockResolvedValue(full);
      let issuance = 0;
      mocks.beginDesktopPlaybackSessionV1.mockImplementation(
        async (segmentId: string, _grant: string, expectedRevision: number) => ({
          playbackReceiptId: `receipt-attempt-${++issuance}`,
          segmentId,
          segmentRevision: expectedRevision,
          clipDurationMs: 1_000,
          expiresAtMs: Date.now() + 60_000,
        }),
      );
      mocks.recordPlaybackReceipt
        .mockRejectedValueOnce({
          schema: 1,
          code,
          message: 'the source or grant changed',
          retryable: true,
          suggestedAction: 'reloadClip',
        })
        .mockImplementationOnce(async (request) => ({
          playbackReceiptId: request.playbackReceiptId,
          segmentId: full.id,
          segmentRevision: 0,
          uniquePlayedMs: 900,
          clipDurationMs: 1_000,
          coverageRatio: 0.9,
        }));

      render(ReviewMode);
      expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
      await hearCurrentAudio();
      const accept = screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) });
      await fireEvent.click(accept);
      await waitFor(() => expect(mocks.recordPlaybackReceipt).toHaveBeenCalledTimes(1));
      expect(mocks.commitReviewV1).not.toHaveBeenCalled();
      await waitFor(() => expect(mocks.beginDesktopPlaybackSessionV1).toHaveBeenCalledTimes(2));

      await hearCurrentAudio();
      await fireEvent.click(accept);
      await waitFor(() => expect(mocks.recordPlaybackReceipt).toHaveBeenCalledTimes(2));
      expect(mocks.recordPlaybackReceipt.mock.calls[0][0]).toMatchObject({
        playbackReceiptId: 'receipt-attempt-1',
      });
      expect(mocks.recordPlaybackReceipt.mock.calls[1][0]).toMatchObject({
        playbackReceiptId: 'receipt-attempt-2',
      });
      await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledTimes(1));
    },
  );

  it('freezes navigation during a truth write and advances only from the authoritative reload', async () => {
    const first = segment();
    const second: SpeechSegment = {
      ...segment(),
      id: 'review-2',
      rawTranscript: 'دەقی دووەم',
      audioPath: 'C:\\audio\\review-2.wav',
    };
    let resolveDecision!: (value: ReturnType<typeof decisionCommit>) => void;
    mocks.getDatasetStats.mockResolvedValue({ totalSegments: 2, verifiedCount: 0 });
    mocks.getSegmentsPage.mockResolvedValue({
      items: [
        { ...first, alignmentJson: null, evidenceJson: null },
        { ...second, alignmentJson: null, evidenceJson: null },
      ],
      total: 2,
      nextCursor: null,
    });
    mocks.getSegment.mockImplementation((id: string) =>
      Promise.resolve(id === first.id ? first : second),
    );
    mocks.recordHumanDecision.mockReturnValue(
      new Promise<ReturnType<typeof decisionCommit>>((resolve) => {
        resolveDecision = resolve;
      }),
    );

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toHaveValue(first.rawTranscript);
    await hearCurrentAudio();

    await fireEvent.click(
      screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) }),
    );
    await waitFor(() =>
      expect(mocks.recordHumanDecision).toHaveBeenCalledWith(
        first.id,
        'accept',
        first.rawTranscript,
      ),
    );

    // A truth write owns renderer-wide selection authority until the backend outcome and every
    // projection have settled. A shortcut cannot move the editor underneath that immutable intent.
    await fireEvent.keyDown(window, { key: 'ArrowRight', code: 'ArrowRight' });
    expect(screen.getByRole('textbox')).toHaveValue(first.rawTranscript);

    resolveDecision(decisionCommit(first, 'accept', first.rawTranscript));
    await waitFor(() => expect(mocks.updateSegmentMetadataV1).not.toHaveBeenCalled());
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue(second.rawTranscript));
    expect(screen.getByTestId('review-source-file')).toHaveTextContent('review-2.wav');
  });

  it('keeps navigation drafts in session memory and never persists them without a decision', async () => {
    const first = segment();
    const second: SpeechSegment = {
      ...segment(),
      id: 'review-2',
      rawTranscript: 'دەقی دووەم',
      audioPath: 'C:\\audio\\review-2.wav',
    };
    mocks.getDatasetStats.mockResolvedValue({ totalSegments: 2, verifiedCount: 0 });
    mocks.getSegmentsPage.mockResolvedValue({
      items: [
        { ...first, alignmentJson: null, evidenceJson: null },
        { ...second, alignmentJson: null, evidenceJson: null },
      ],
      total: 2,
      nextCursor: null,
    });
    mocks.getSegment.mockImplementation((id: string) =>
      Promise.resolve(id === first.id ? first : second),
    );

    const view = render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'دەقی دەستکاریکراو' } });

    await fireEvent.keyDown(window, { key: 'ArrowRight', code: 'ArrowRight' });
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue(second.rawTranscript));
    expect(mocks.updateSegmentMetadataV1).not.toHaveBeenCalled();
    expect(mocks.recordHumanDecision).not.toHaveBeenCalled();

    await fireEvent.keyDown(window, { key: 'ArrowLeft', code: 'ArrowLeft' });
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue('دەقی دەستکاریکراو'));
    expect(mocks.updateSegmentMetadataV1).not.toHaveBeenCalled();

    view.unmount();
    expect(mocks.updateSegmentMetadataV1).not.toHaveBeenCalled();
    expect(mocks.recordHumanDecision).not.toHaveBeenCalled();
  });

  it('aligns only with hydrated chunk metadata and force-refreshes the persisted result', async () => {
    settings.set({ ...defaultSettings, autoAlign: true });
    const chunkJson = JSON.stringify({
      source_start_ms: 4_000,
      source_end_ms: 5_000,
      chunk_index: 1,
      chunk_count: 2,
      words: [{ word: 'دەقی', start: 0, end: 0.4, confidence: 0.7 }],
    });
    const heuristic = {
      ...segment(),
      alignmentJson: chunkJson,
      alignmentQuality: 'energy_heuristic',
    };
    const refreshed = { ...heuristic, alignmentQuality: 'ctc_forced' };
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...heuristic, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
    });
    mocks.getSegment.mockResolvedValueOnce(heuristic).mockResolvedValueOnce(refreshed);
    mocks.alignSegment.mockResolvedValue([{ word: 'دەقی', start: 0, end: 0.4, confidence: 0.9 }]);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await waitFor(() =>
      expect(mocks.alignSegment).toHaveBeenCalledWith(
        heuristic.audioPath,
        heuristic.rawTranscript,
        chunkJson,
        heuristic.id,
      ),
    );
    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledTimes(2));
  });
  it('falls through a BLANK annotated column to the champion raw draft', async () => {
    // Verbatim review precedence is human annotation ▸ champion raw, and blank optionals are ABSENT.
    // `??` only falls through on null, so a whitespace-only annotated row masked the champion draft
    // and opened an EMPTY editor — the reviewer then retypes (or accepts) text nobody drafted.
    // Fail-before: the editor holds '   ' here.
    const blank = { ...segment(), annotatedTranscript: '   ' };
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...blank, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
    });
    mocks.getSegment.mockResolvedValue(blank);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toHaveValue(blank.rawTranscript);
  });

  it('does not let a durable machine refinement replace the champion raw review draft', async () => {
    const refined = {
      ...segment(),
      rawTranscript: 'دەقی خامی چەمپیۆن',
      normalizedTranscript: 'دەقی کۆتایی ڕێکخراو',
    };
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...refined, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
    });
    mocks.getSegment.mockResolvedValue(refined);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toHaveValue(refined.rawTranscript);
  });

  it('still prefers a real annotated human transcript over every machine projection', async () => {
    const annotated = {
      ...segment(),
      normalizedTranscript: 'دەقی کۆتایی ماشین',
      annotatedTranscript: 'دەقی مرۆیی',
    };
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...annotated, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
    });
    mocks.getSegment.mockResolvedValue(annotated);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toHaveValue('دەقی مرۆیی');
  });

  it('refuses to record a verdict when the clip audio could not be played', async () => {
    // AUDIT FIND 2026-08-17: the player showed its error banner while Accept/Save stayed live, so a
    // clip whose audio failed (missing permission, corrupt container, decode failure) could be marked
    // human-verified by someone who never heard it. `speech_segments` cannot tell that apart from a
    // real listen, and this is a VERBATIM corpus — the queue already refuses clips whose FILE is gone
    // (2026-08-15); this covers every other failure mode. Fail-before: without the guard,
    // recordHumanDecision IS called here.
    const seg: SpeechSegment = {
      ...segment(),
      id: 'unplayable-1',
      audioPath: 'C:\\audio\\gone.wav',
    };
    mocks.getSegmentsPage.mockResolvedValue({ items: [seg], total: 1, nextCursor: null });
    mocks.getSegment.mockResolvedValue(seg);
    // The real failure path: resolving the playable URL throws, so AudioPlayer sets its error state.
    mocks.getMediaAssetUrl.mockRejectedValue(new Error('audio unavailable'));
    mocks.registerMediaAsset.mockRejectedValue(new Error('audio unavailable'));

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();

    const accept = await screen.findByText(new RegExp(ckb['review.acceptAsIs']));
    await fireEvent.click(accept);
    // The refusal is the assertion: nothing was written.
    await waitFor(() => expect(mocks.recordHumanDecision).not.toHaveBeenCalled());
    expect(mocks.updateSegmentMetadataV1).not.toHaveBeenCalled();
  });

  it('requires an explicit technical reason, retains the clip on a definitive failure, and advances only after a verified unusable commit', async () => {
    const failed = {
      ...segment(),
      id: 'unplayable-1',
      audioPath: 'C:\\audio\\gone.wav',
      rawTranscript: 'ڕەشنووسی پارێزراو',
    };
    const next = {
      ...segment(),
      id: 'playable-2',
      audioPath: 'C:\\audio\\next.wav',
      rawTranscript: 'پارچەی دواتر',
    };
    mocks.getSegmentsPage.mockResolvedValue({
      items: [failed, next],
      total: 2,
      nextCursor: null,
      revisions: { [failed.id]: 4, [next.id]: 9 },
    });
    mocks.getSegment.mockImplementation(async (id: string) => (id === failed.id ? failed : next));
    mocks.registerReviewMediaAsset.mockImplementation(async (path: string) => {
      if (path === failed.audioPath) throw new Error('file missing');
      return { id: NEXT_MEDIA_GRANT_ID };
    });
    mocks.markSegmentUnusableV1
      .mockRejectedValueOnce({
        schema: 1,
        code: 'WRITE_REJECTED',
        message: 'the backend proved no write occurred',
        retryable: true,
      })
      .mockImplementationOnce(async (request) => {
        removedReviewSegmentIds.add(request.segmentId);
        undoAvailability = {
          status: 'available',
          target: technicalFlagUndoTarget(
            request.segmentId,
            request.operationId,
            request.baseRevision,
            request.reason,
            202,
          ),
        };
        return {
          segmentId: request.segmentId,
          committedRevision: request.baseRevision + 1,
          reason: request.reason,
          effectId: 'flag-effect:202',
        };
      });

    render(ReviewMode);
    expect(await screen.findByTestId('review-technical-unusable')).toBeInTheDocument();
    const editor = screen.getByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'ڕەشنووسی نەپاشەکەوتکراو' } });
    const reason = screen.getByLabelText(ckb['review.unusable.reasonLabel']);
    const mark = screen.getByRole('button', { name: ckb['review.unusable.mark'] });
    expect(mark).toBeDisabled();
    reason.focus();
    await fireEvent.keyDown(reason, { key: 'ArrowDown', code: 'ArrowDown' });
    expect(screen.getByTestId('review-source-file')).toHaveTextContent('gone.wav');
    await fireEvent.change(reason, { target: { value: 'missingFile' } });
    // A technical disposition preserves revision-bound drafts. A visible correction still requires
    // explicit reset here so the reviewer cannot accidentally classify a clip while editing it.
    expect(mark).toBeDisabled();
    expect(mark).toHaveAttribute(
      'aria-describedby',
      'review-unusable-help review-reject-disabled-reason',
    );
    await fireEvent.click(screen.getByRole('button', { name: ckb['review.reset'] }));
    const resetConfirmation = get(showConfirmDialog);
    expect(resetConfirmation).toMatchObject({
      title: ckb['review.resetConfirmTitle'],
      message: ckb['review.resetConfirmMessage'],
      confirmLabel: ckb['review.resetConfirmAction'],
      danger: true,
    });
    expect(mark).toBeDisabled();
    expect(mocks.deleteReviewDraftV1).not.toHaveBeenCalled();
    showConfirmDialog.set(null);
    await resetConfirmation?.onConfirm();
    await waitFor(() => expect(mark).toBeEnabled());
    expect(editor).toHaveValue(failed.rawTranscript);
    // The edit never reached its debounce deadline, so the coordinator proves the already-durable
    // no-draft baseline locally instead of issuing a redundant native deletion.
    expect(mocks.deleteReviewDraftV1).not.toHaveBeenCalled();

    mark.focus();
    await fireEvent.click(mark);
    await waitFor(() => expect(mocks.markSegmentUnusableV1).toHaveBeenCalledTimes(1));
    expect(editor).toHaveValue(failed.rawTranscript);
    expect(document.activeElement).toBe(mark);
    expect(screen.getByTestId('review-source-file')).toHaveTextContent('gone.wav');
    expect(mocks.recordPlaybackReceipt).not.toHaveBeenCalled();
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();

    mocks.saveReviewDraftV1.mockClear();
    await fireEvent.click(mark);
    await waitFor(() => expect(mocks.markSegmentUnusableV1).toHaveBeenCalledTimes(2));
    const first = mocks.markSegmentUnusableV1.mock.calls[0][0];
    const replay = mocks.markSegmentUnusableV1.mock.calls[1][0];
    expect(first).toEqual({
      operationId: expect.stringMatching(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
      ),
      segmentId: failed.id,
      baseRevision: 4,
      reason: 'missingFile',
    });
    expect(replay).toEqual(first);
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue(next.rawTranscript));
    expect(screen.getByTestId('review-source-file')).toHaveTextContent('next.wav');
    await new Promise((resolve) => setTimeout(resolve, 550));
    expect(mocks.saveReviewDraftV1).not.toHaveBeenCalled();
  });

  it.each(['STALE_REVISION', 'HUMAN_TRUTH_ALREADY_COMMITTED', 'SEGMENT_NOT_FOUND'])(
    'reloads %s technical-unusable authority and retries with the new revision and a new operation identity',
    async (refusalCode) => {
      const failed = {
        ...segment(),
        id: 'unplayable-stale-1',
        audioPath: 'C:\\audio\\stale-gone.wav',
        rawTranscript: 'ڕەشنووسی پارێزراو',
      };
      const next = {
        ...segment(),
        id: 'unplayable-stale-next-2',
        audioPath: 'C:\\audio\\stale-next.wav',
        rawTranscript: 'پارچەی دواتر',
      };
      let failedRevision = 4;
      mocks.getSegmentsPage.mockImplementation(async () => ({
        items: [failed, next],
        total: 2,
        nextCursor: null,
        revisions: { [failed.id]: failedRevision, [next.id]: 9 },
      }));
      mocks.getSegment.mockImplementation(async (id: string) => (id === failed.id ? failed : next));
      mocks.registerReviewMediaAsset.mockImplementation(async (path: string) => {
        if (path === failed.audioPath) throw new Error('file missing');
        return { id: NEXT_MEDIA_GRANT_ID };
      });
      mocks.markSegmentUnusableV1
        .mockImplementationOnce(async () => {
          failedRevision = 5;
          throw {
            schema: 1,
            code: refusalCode,
            message: 'This clip changed; reload it before marking it unusable.',
            retryable: false,
            suggestedAction: 'reloadClip',
            details: { expectedRevision: 4, currentRevision: 5 },
          };
        })
        .mockImplementationOnce(async (request) => {
          removedReviewSegmentIds.add(request.segmentId);
          undoAvailability = {
            status: 'available',
            target: technicalFlagUndoTarget(
              request.segmentId,
              request.operationId,
              request.baseRevision,
              request.reason,
              203,
            ),
          };
          return {
            segmentId: request.segmentId,
            committedRevision: request.baseRevision + 1,
            reason: request.reason,
            effectId: 'flag-effect:203',
          };
        });

      render(ReviewMode);
      expect(await screen.findByTestId('review-technical-unusable')).toBeInTheDocument();
      await fireEvent.change(screen.getByLabelText(ckb['review.unusable.reasonLabel']), {
        target: { value: 'missingFile' },
      });
      await fireEvent.click(screen.getByRole('button', { name: ckb['review.unusable.mark'] }));

      await waitFor(() => expect(mocks.getSegmentsPage).toHaveBeenCalledTimes(2));
      await waitFor(() =>
        expect(screen.getByTestId('review-source-file')).toHaveTextContent('stale-gone.wav'),
      );
      expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(false);

      await fireEvent.change(screen.getByLabelText(ckb['review.unusable.reasonLabel']), {
        target: { value: 'missingFile' },
      });
      await fireEvent.click(screen.getByRole('button', { name: ckb['review.unusable.mark'] }));

      await waitFor(() => expect(mocks.markSegmentUnusableV1).toHaveBeenCalledTimes(2));
      const first = mocks.markSegmentUnusableV1.mock.calls[0][0];
      const retry = mocks.markSegmentUnusableV1.mock.calls[1][0];
      expect(first).toMatchObject({ segmentId: failed.id, baseRevision: 4, reason: 'missingFile' });
      expect(retry).toMatchObject({ segmentId: failed.id, baseRevision: 5, reason: 'missingFile' });
      expect(retry.operationId).not.toBe(first.operationId);
      await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue(next.rawTranscript));
    },
  );
});
