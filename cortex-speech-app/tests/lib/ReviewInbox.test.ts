import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

const api = vi.hoisted(() => ({
  getReviewPageV1: vi.fn(),
  getSegmentsPage: vi.fn(),
  getDatasetCertificate: vi.fn(),
  getDatasetStats: vi.fn(),
  getReviewDraftV1: vi.fn(),
  saveReviewDraftV1: vi.fn(),
  deleteReviewDraftV1: vi.fn(),
  getSettings: vi.fn(),
  updateSettings: vi.fn(),
  getSegmentIdsForView: vi.fn(),
  runJuryPipeline: vi.fn(),
  recordPlaybackReceipt: vi.fn(),
  commitReviewV1: vi.fn(),
  markSegmentUnusableV1: vi.fn(),
  recordReviewFlag: vi.fn(),
  getDesktopReviewUndoAvailabilityV1: vi.fn(),
  undoDesktopReviewActionV1: vi.fn(),
  reviewEffectId: vi.fn(),
  registerMediaAsset: vi.fn(),
  registerReviewMediaAsset: vi.fn(),
  getMediaAssetUrl: vi.fn(),
  beginDesktopPlaybackSessionV1: vi.fn(),
  cancelDesktopPlaybackSessionV1: vi.fn(),
  isCommandErrorV1: vi.fn(
    (error: unknown, code?: string) =>
      !!error &&
      typeof error === 'object' &&
      (error as { schema?: number }).schema === 1 &&
      (code === undefined || (error as { code?: string }).code === code),
  ),
  reviewErrorMessage: vi.fn((_error: unknown, fallback: string) => fallback),
}));

vi.mock('../../src/lib/commands', () => api);

import ReviewInbox from '../../src/lib/ReviewInbox.svelte';
import { createReviewInboxDecisionController } from '../../src/lib/reviewInboxDecisions.svelte';
import { locale } from '../../src/lib/i18n';
import { ckb } from '../../src/lib/i18n/ckb';
import { en } from '../../src/lib/i18n/en';
import { safeInboxEvidence } from '../../src/lib/reviewInboxDecisions.svelte';
import {
  createDurableReviewUndoController,
  sharedDurableReviewUndo,
} from '../../src/lib/durableReviewUndo.svelte';
import {
  REVIEW_OPERATION_TIMEOUT_MS,
  withReviewOperationTimeout,
} from '../../src/lib/reviewOperationTimeout';
import { defaultSettings } from '../../src/lib/stores/settingsStore';
import { showConfirmDialog } from '../../src/lib/stores/uiStore';
import type { SpeechSegment } from '../../src/lib/types';

const MEDIA_GRANT_ID = '2f2d9b66-8566-4d1c-8c14-e18d006b776f';
const NEXT_MEDIA_GRANT_ID = '52a492d4-14d8-4e24-9f5d-bc44221b48c1';
const UNDO_PAYLOAD_HASH = 'b'.repeat(64);

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
  flagKind:
    | { kind: 'generic' }
    | {
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

function flagUndoTarget(
  segmentId: string,
  sourceOperationId: string,
  priorRevision: number,
  flagKind: FlagUndoTarget['flagKind'],
  effectEventId: number,
): FlagUndoTarget {
  return {
    kind: 'flag',
    effectEventId,
    segmentId,
    sourceOperationId,
    sourcePayloadHash: 'c'.repeat(64),
    priorRevision,
    flagRevision: priorRevision + 1,
    flagKind,
    databaseGeneration: 1,
  };
}

function inboxSegment(id: string, withAudio = false): SpeechSegment {
  return {
    id,
    audioPath: withAudio ? `C:\\audio\\${id}.wav` : '',
    rawTranscript: 'دەق',
    normalizedTranscript: null,
    annotatedTranscript: null,
    alignmentJson: JSON.stringify({ source_start_ms: 0, source_end_ms: 1_000 }),
    durationMs: 1000,
    speakerId: null,
    verified: false,
  };
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

function reviewPage(
  segments: SpeechSegment[],
  options: {
    baseRevision?: number;
    eligible?: boolean;
    disabledReason?: string | null;
    total?: number;
    nextCursor?: string | null;
  } = {},
) {
  const baseRevision = options.baseRevision ?? 7;
  const eligible = options.eligible ?? true;
  return {
    items: segments.map((segment) => ({
      segment,
      baseRevision,
      eligible,
      disabledReason: options.disabledReason ?? null,
    })),
    total: options.total ?? segments.length,
    nextCursor: options.nextCursor ?? null,
    scopeLabel: 'escalation',
    focusNarrowed: false,
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

describe('ReviewInbox queue loading', () => {
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
    showConfirmDialog.set(null);
    installPlayableMediaStub();
    api.cancelDesktopPlaybackSessionV1.mockResolvedValue(true);
    api.getReviewDraftV1.mockResolvedValue(null);
    api.saveReviewDraftV1.mockImplementation(
      async (segmentId: string, baseRevision: number, text: string) => ({
        segmentId,
        baseRevision,
        text,
        updatedAt: '2026-08-26T00:00:00Z',
      }),
    );
    api.deleteReviewDraftV1.mockResolvedValue(true);
    api.getSettings.mockResolvedValue(null);
    api.updateSettings.mockResolvedValue(undefined);
    api.registerMediaAsset.mockResolvedValue({ id: MEDIA_GRANT_ID });
    api.registerReviewMediaAsset.mockResolvedValue({ id: MEDIA_GRANT_ID });
    api.getMediaAssetUrl.mockImplementation(
      async (id: string) => `http://cortex-media.localhost/${id}`,
    );
    api.beginDesktopPlaybackSessionV1.mockImplementation(
      async (segmentId: string, _mediaGrantId: string, expectedRevision: number) => ({
        playbackReceiptId: `receipt-${segmentId}`,
        segmentId,
        segmentRevision: expectedRevision,
        clipDurationMs: 1000,
        expiresAtMs: Date.now() + 60_000,
      }),
    );
    api.recordPlaybackReceipt.mockImplementation(
      async ({ playbackReceiptId }: { playbackReceiptId: string }) => ({
        playbackReceiptId,
        segmentId: playbackReceiptId.replace(/^receipt-/, ''),
        segmentRevision: 7,
        uniquePlayedMs: 1000,
        clipDurationMs: 1000,
        coverageRatio: 1,
      }),
    );
    api.getSegmentsPage.mockResolvedValue({ items: [], total: 0, nextCursor: null });
    api.getDatasetCertificate.mockResolvedValue({ threshold: 0.35 });
    api.getDatasetStats.mockResolvedValue({
      totalSegments: 1,
      verifiedCount: 0,
      pendingCount: 1,
      totalDurationSeconds: 1,
    });
    api.commitReviewV1.mockImplementation(
      async (request: {
        operationId: string;
        segmentId: string;
        baseRevision: number;
        decision: UndoDecision;
        transcript: string | null;
      }) => {
        undoAvailability = {
          status: 'available',
          target: undoTarget(request.segmentId, request.decision, request.operationId),
        };
        return {
          segmentId: request.segmentId,
          committedRevision: request.baseRevision + 1,
          authoritativeTranscript: request.transcript ?? 'دەق',
          decisionId: 'effect:101',
        };
      },
    );
    api.markSegmentUnusableV1.mockImplementation(
      async (request: {
        operationId: string;
        segmentId: string;
        baseRevision: number;
        reason: 'decodeFailed' | 'missingFile' | 'permissionDenied' | 'corruptContainer';
      }) => {
        undoAvailability = {
          status: 'available',
          target: flagUndoTarget(
            request.segmentId,
            request.operationId,
            request.baseRevision,
            { kind: 'technicalUnusable', reason: request.reason },
            303,
          ),
        };
        return {
          segmentId: request.segmentId,
          committedRevision: request.baseRevision + 1,
          reason: request.reason,
          effectId: 'flag-effect:303',
        };
      },
    );
    api.recordReviewFlag.mockImplementation(
      async (request: {
        operationId: string;
        segmentId: string;
        baseRevision: number;
        rationale: string;
      }) => {
        undoAvailability = {
          status: 'available',
          target: flagUndoTarget(
            request.segmentId,
            request.operationId,
            request.baseRevision,
            { kind: 'generic' },
            202,
          ),
        };
        return {
          segment: { ...inboxSegment(request.segmentId), escalated: true },
          segmentId: request.segmentId,
          effectEventId: 202,
          priorRevision: request.baseRevision,
          flagRevision: request.baseRevision + 1,
        };
      },
    );
    api.getDesktopReviewUndoAvailabilityV1.mockImplementation(async () => {
      if (undoAvailability.status === 'none') {
        const request = api.commitReviewV1.mock.calls.at(-1)?.[0] as
          { operationId?: string; segmentId?: string; decision?: UndoDecision } | undefined;
        const decisionId = api.reviewEffectId.mock.calls.at(-1)?.[0] as string | undefined;
        const effect = /^effect:([1-9][0-9]*)$/.exec(decisionId ?? '');
        if (request?.operationId && request.segmentId && request.decision && effect !== null) {
          return {
            status: 'available',
            target: undoTarget(
              request.segmentId,
              request.decision,
              request.operationId,
              Number(effect[1]),
            ),
          };
        }
      }
      return undoAvailability;
    });
    api.undoDesktopReviewActionV1.mockImplementation(async (target: UndoTarget) => {
      undoAvailability = {
        status: 'blocked',
        reason: target.kind === 'flag' ? 'latestFlagUndone' : 'latestDecisionUndone',
      };
      return {
        status: 'applied',
        effectKind: target.kind,
        effectEventId: target.effectEventId,
        restoredRevision: 9,
        segment: inboxSegment(target.segmentId),
      };
    });
    api.reviewEffectId.mockImplementation((decisionId: string) => {
      const match = /^effect:([1-9][0-9]*)$/.exec(decisionId);
      return match ? Number(match[1]) : null;
    });
    Element.prototype.scrollIntoView = vi.fn();
    locale.set('en');
  });

  afterEach(cleanup);

  it('shows a retryable error instead of claiming an unread queue is empty', async () => {
    api.getReviewPageV1.mockRejectedValueOnce(new Error('database unavailable'));
    api.getReviewPageV1.mockResolvedValueOnce(reviewPage([]));

    render(ReviewInbox);

    const alert = await screen.findByTestId('review-inbox-load-error');
    expect(alert).toHaveTextContent('Could not load the review queue');
    expect(alert).toHaveTextContent('An unknown review error occurred');
    expect(alert).not.toHaveTextContent('database unavailable');
    expect(screen.queryByText('Inbox zero!')).not.toBeInTheDocument();
    expect(screen.getByRole('dialog', { name: 'Review Inbox' })).toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() =>
      expect(screen.queryByTestId('review-inbox-load-error')).not.toBeInTheDocument(),
    );
    expect(await screen.findByText('Inbox zero!')).toBeInTheDocument();
  });

  it.each([
    ['champion raw', null, 'دەقی خامی چەمپیۆن'],
    ['human annotation', 'دەقی نووسراوی مرۆڤ', 'دەقی نووسراوی مرۆڤ'],
  ])(
    'keeps a machine jury verdict out of the %s draft and accept intent',
    async (_case, annotatedTranscript, expectedTranscript) => {
      const row = {
        ...inboxSegment('verbatim-law', true),
        rawTranscript: 'دەقی خامی چەمپیۆن',
        normalizedTranscript: 'دەقی ڕەوانکراوی ماشین',
        annotatedTranscript,
        verdict: 'jury_accept',
        verdictTranscript: 'دەقی پێشنیاری ژووری ماشین',
      };
      api.getReviewPageV1.mockResolvedValue(reviewPage([row]));

      render(ReviewInbox);
      await screen.findByText('Queue (1)');
      await waitFor(() => expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalled());
      await hearCurrentAudio();
      await fireEvent.click(screen.getByRole('button', { name: /^A Accept$/ }));

      await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledTimes(1));
      expect(api.commitReviewV1).toHaveBeenCalledWith(
        expect.objectContaining({
          decision: 'accept',
          transcript: expectedTranscript,
        }),
      );
    },
  );

  it('continues the escalation cursor explicitly and caps residency at three pages', async () => {
    const rows = (start: number, count: number) =>
      Array.from({ length: count }, (_, offset) =>
        inboxSegment(`row-${String(start + offset).padStart(4, '0')}`),
      );
    api.getReviewPageV1
      .mockResolvedValueOnce(reviewPage(rows(1, 200), { total: 800, nextCursor: 'cursor-1' }))
      .mockResolvedValueOnce(reviewPage(rows(201, 200), { total: 800, nextCursor: 'cursor-2' }))
      .mockResolvedValueOnce(reviewPage(rows(401, 200), { total: 800, nextCursor: 'cursor-3' }))
      .mockResolvedValueOnce(reviewPage(rows(601, 200), { total: 800, nextCursor: null }));

    render(ReviewInbox);
    expect(await screen.findByText('200 of 800 escalation rows loaded')).toBeInTheDocument();

    for (const [index, expectedLoaded] of [400, 600, 600].entries()) {
      await fireEvent.click(screen.getByRole('button', { name: 'Load more' }));
      await waitFor(() => expect(api.getReviewPageV1).toHaveBeenCalledTimes(index + 2));
      if (index < 2) {
        await waitFor(() =>
          expect(
            screen.getByText(`${expectedLoaded} of 800 escalation rows loaded`),
          ).toBeInTheDocument(),
        );
      } else {
        await waitFor(() => expect(screen.queryByRole('button', { name: 'Load more' })).toBeNull());
      }
    }

    expect(api.getReviewPageV1.mock.calls.slice(1).map((call) => call[1])).toEqual([
      'cursor-1',
      'cursor-2',
      'cursor-3',
    ]);
    expect(within(screen.getByRole('listbox')).getAllByRole('option')).toHaveLength(600);
    expect(screen.getByText('row-0001…')).toBeInTheDocument();
    expect(screen.queryByText('row-0002…')).not.toBeInTheDocument();
    expect(screen.getByText('row-0800…')).toBeInTheDocument();
    expect(screen.getByTestId('inbox-eviction-notice')).toHaveTextContent(
      '200 earlier rows were released',
    );
    expect(screen.getByRole('button', { name: 'Reload from start' })).toBeInTheDocument();
  });

  it('retains the resident queue when loading the next page fails and retries the same cursor', async () => {
    const first = inboxSegment('aaaaaaaa-1');
    const second = inboxSegment('bbbbbbbb-2');
    api.getReviewPageV1
      .mockResolvedValueOnce(reviewPage([first], { total: 2, nextCursor: 'cursor-1' }))
      .mockRejectedValueOnce(new Error('page database unavailable'))
      .mockResolvedValueOnce(reviewPage([second], { total: 2, nextCursor: null }));

    render(ReviewInbox);
    expect(await screen.findByText('1 of 2 escalation rows loaded')).toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: 'Load more' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('An unknown review error occurred');
    expect(screen.getByText('aaaaaaaa…')).toBeInTheDocument();
    expect(screen.queryByText('bbbbbbbb…')).not.toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: 'Load more' }));
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Load more' })).toBeNull());
    expect(screen.getByText('aaaaaaaa…')).toBeInTheDocument();
    expect(screen.getByText('bbbbbbbb…')).toBeInTheDocument();
    expect(api.getReviewPageV1.mock.calls.slice(1).map((call) => call[1])).toEqual([
      'cursor-1',
      'cursor-1',
    ]);
  });

  it('never lets a duplicate later page replace the selected revision or active correction', async () => {
    const selected = { ...inboxSegment('selected', true), rawTranscript: 'server text revision 7' };
    const duplicate = {
      ...selected,
      rawTranscript: 'concurrently changed server text revision 8',
    };
    api.getReviewPageV1
      .mockResolvedValueOnce(
        reviewPage([selected, inboxSegment('row-2')], {
          baseRevision: 7,
          total: 3,
          nextCursor: 'cursor-1',
        }),
      )
      .mockResolvedValueOnce(
        reviewPage([duplicate, inboxSegment('row-3')], {
          baseRevision: 8,
          total: 3,
          nextCursor: null,
        }),
      );

    render(ReviewInbox);
    expect(await screen.findByText('server text revision 7')).toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    const editor = screen.getByLabelText(/Edit transcript/);
    expect(editor).toHaveValue('server text revision 7');
    await fireEvent.input(editor, { target: { value: 'new human correction' } });

    await fireEvent.click(screen.getByRole('button', { name: 'Load more' }));
    await waitFor(() => expect(api.getReviewPageV1).toHaveBeenCalledTimes(2));
    expect(editor).toHaveValue('new human correction');
    expect(screen.queryByText('concurrently changed server text revision 8')).toBeNull();

    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /Save edit/ }));
    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledTimes(1));
    expect(api.commitReviewV1).toHaveBeenCalledWith(
      expect.objectContaining({
        segmentId: selected.id,
        baseRevision: 7,
        transcript: 'new human correction',
      }),
    );
  });

  it('drops a late draft hydration result after navigation to another segment', async () => {
    const first = inboxSegment('aaaaaaaa-1');
    const second = inboxSegment('bbbbbbbb-2');
    let resolveFirstDraft!: (value: {
      segmentId: string;
      baseRevision: number;
      text: string;
      updatedAt: string;
    }) => void;
    api.getReviewPageV1.mockResolvedValue(reviewPage([first, second]));
    api.getReviewDraftV1
      .mockReturnValueOnce(
        new Promise((resolve) => {
          resolveFirstDraft = resolve;
        }),
      )
      .mockResolvedValueOnce(null);

    render(ReviewInbox);
    const listbox = await screen.findByRole('listbox', { name: 'Queue (2)' });
    const options = within(listbox).getAllByRole('option');
    await fireEvent.click(options[1]);
    await waitFor(() => expect(options[1]).toHaveAttribute('aria-selected', 'true'));

    resolveFirstDraft({
      segmentId: first.id,
      baseRevision: 7,
      text: 'draft for the first row',
      updatedAt: '2026-08-26T00:00:00Z',
    });
    await Promise.resolve();

    expect(options[1]).toHaveAttribute('aria-selected', 'true');
    expect(screen.queryByDisplayValue('draft for the first row')).not.toBeInTheDocument();
  });

  it('recovers a matching durable draft into the editor without merging revisions', async () => {
    const row = inboxSegment('aaaaaaaa-1');
    api.getReviewPageV1.mockResolvedValue(reviewPage([row], { baseRevision: 7 }));
    api.getReviewDraftV1.mockResolvedValue({
      segmentId: row.id,
      baseRevision: 7,
      text: 'دەقی گەڕێندراوە',
      updatedAt: '2026-08-26T00:00:00Z',
    });

    render(ReviewInbox);

    expect(await screen.findByRole('textbox')).toHaveValue('دەقی گەڕێندراوە');
    expect(
      screen.getByText('Recovered your unsaved draft from this workstation.'),
    ).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^A Accept$/ })).toBeDisabled();
  });

  it('retries failed draft recovery before enabling review actions', async () => {
    const row = inboxSegment('draft-retry');
    api.getReviewPageV1.mockResolvedValue(reviewPage([row]));
    api.getReviewDraftV1
      .mockRejectedValueOnce(new Error('draft store unavailable'))
      .mockResolvedValueOnce(null);

    render(ReviewInbox);
    expect(
      await screen.findByText(
        'The current server transcript is shown. Reload the clip to retry draft recovery.',
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^A Accept$/ })).toBeDisabled();

    await fireEvent.click(screen.getByRole('button', { name: 'Retry draft recovery' }));
    await waitFor(() => expect(api.getReviewDraftV1).toHaveBeenCalledTimes(2));
    await waitFor(() =>
      expect(
        screen.queryByText(
          'The current server transcript is shown. Reload the clip to retry draft recovery.',
        ),
      ).toBeNull(),
    );
    expect(screen.getByRole('button', { name: /^A Accept$/ })).toBeEnabled();
  });

  it('requires an explicit choice before carrying a stale draft onto the current revision', async () => {
    const row = { ...inboxSegment('stale-draft'), rawTranscript: 'current server truth' };
    api.getReviewPageV1.mockResolvedValue(reviewPage([row], { baseRevision: 7 }));
    api.getReviewDraftV1.mockResolvedValue({
      segmentId: row.id,
      baseRevision: 6,
      text: 'saved local correction',
      updatedAt: '2026-08-26T00:00:00Z',
    });

    render(ReviewInbox);
    expect(await screen.findByText('Saved draft needs your decision')).toBeInTheDocument();
    expect(screen.getAllByText('current server truth')).toHaveLength(2);
    expect(screen.getByText('saved local correction')).toBeInTheDocument();
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: 'Use saved draft' }));
    const editor = await screen.findByRole('textbox');
    expect(editor).toHaveValue('saved local correction');
    await fireEvent.click(screen.getByRole('button', { name: 'Keep draft (Esc)' }));
    await waitFor(() =>
      expect(api.saveReviewDraftV1).toHaveBeenCalledWith(row.id, 7, 'saved local correction'),
    );
    expect(await screen.findByText('Correction kept as a recovery draft.')).toBeInTheDocument();
    expect(api.commitReviewV1).not.toHaveBeenCalled();
  });

  it('requires global confirmation before discarding the exact stale draft revision', async () => {
    const row = { ...inboxSegment('stale-draft-discard'), rawTranscript: 'current server truth' };
    api.getReviewPageV1.mockResolvedValue(reviewPage([row], { baseRevision: 7 }));
    api.getReviewDraftV1.mockResolvedValue({
      segmentId: row.id,
      baseRevision: 6,
      text: 'saved local correction',
      updatedAt: '2026-08-26T00:00:00Z',
    });

    render(ReviewInbox);
    expect(await screen.findByText('Saved draft needs your decision')).toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: 'Discard saved draft' }));

    const cancelledConfirmation = get(showConfirmDialog);
    expect(cancelledConfirmation).toMatchObject({
      title: en['review.discardDraftConfirmTitle'],
      message: en['review.discardDraftConfirmMessage'],
      confirmLabel: en['review.discardLocalDraft'],
      danger: true,
    });
    expect(api.deleteReviewDraftV1).not.toHaveBeenCalled();
    showConfirmDialog.set(null);
    cancelledConfirmation?.onCancel?.();
    expect(api.deleteReviewDraftV1).not.toHaveBeenCalled();
    expect(screen.getByText('Saved draft needs your decision')).toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: 'Discard saved draft' }));
    const confirmed = get(showConfirmDialog);
    expect(confirmed).not.toBeNull();
    showConfirmDialog.set(null);
    await confirmed?.onConfirm();

    await waitFor(() => expect(api.deleteReviewDraftV1).toHaveBeenCalledWith(row.id, 6));
    expect(screen.queryByText('Saved draft needs your decision')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^A Accept$/ })).toBeEnabled();
  });

  it('refuses a stale draft confirmation after rail selection changes', async () => {
    const first = { ...inboxSegment('stale-draft-first'), rawTranscript: 'first server truth' };
    const second = { ...inboxSegment('stale-draft-second'), rawTranscript: 'second server truth' };
    api.getReviewPageV1.mockResolvedValue(reviewPage([first, second], { baseRevision: 7 }));
    api.getReviewDraftV1.mockImplementation(async (id: string) =>
      id === first.id
        ? {
            segmentId: first.id,
            baseRevision: 6,
            text: 'first saved correction',
            updatedAt: '2026-08-26T00:00:00Z',
          }
        : null,
    );

    render(ReviewInbox);
    expect(await screen.findByText('Saved draft needs your decision')).toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: 'Discard saved draft' }));
    const staleConfirmation = get(showConfirmDialog);
    expect(staleConfirmation).not.toBeNull();

    const options = within(screen.getByRole('listbox', { name: 'Queue (2)' })).getAllByRole(
      'option',
    );
    await fireEvent.click(options[1]);
    await waitFor(() => expect(options[1]).toHaveAttribute('aria-selected', 'true'));
    await waitFor(() => expect(screen.getByText(second.rawTranscript)).toBeInTheDocument());
    showConfirmDialog.set(null);
    await staleConfirmation?.onConfirm();

    expect(api.deleteReviewDraftV1).not.toHaveBeenCalled();
    expect(options[1]).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByText(second.rawTranscript)).toBeInTheDocument();
  });

  it('waits for the durable draft barrier before closing the inbox', async () => {
    const onClose = vi.fn();
    let resolveSave!: (value: {
      segmentId: string;
      baseRevision: number;
      text: string;
      updatedAt: string;
    }) => void;
    api.getReviewPageV1.mockResolvedValue(reviewPage([inboxSegment('aaaaaaaa-1')]));
    api.saveReviewDraftV1.mockReturnValue(
      new Promise((resolve) => {
        resolveSave = resolve;
      }),
    );

    render(ReviewInbox, { onClose });
    await screen.findByText('Queue (1)');
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    await fireEvent.input(screen.getByRole('textbox'), { target: { value: 'durable correction' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Close inbox' }));

    await waitFor(() => expect(api.saveReviewDraftV1).toHaveBeenCalledTimes(1));
    expect(onClose).not.toHaveBeenCalled();
    resolveSave({
      segmentId: 'aaaaaaaa-1',
      baseRevision: 7,
      text: 'durable correction',
      updatedAt: '2026-08-26T00:00:00Z',
    });
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it('keeps the clip, editor and draft when an authoritative commit response is ambiguous', async () => {
    api.getReviewPageV1.mockResolvedValue(reviewPage([inboxSegment('aaaaaaaa-1', true)]));
    api.commitReviewV1.mockRejectedValue(new Error('response lost'));

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await waitFor(() => expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalled());
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    const editor = screen.getByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'دەقی پارێزراو' } });
    await fireEvent.click(screen.getByRole('button', { name: /Save edit/ }));

    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledTimes(1));
    expect(api.saveReviewDraftV1).toHaveBeenCalledWith('aaaaaaaa-1', 7, 'دەقی پارێزراو');
    expect(api.saveReviewDraftV1.mock.invocationCallOrder[0]).toBeLessThan(
      api.commitReviewV1.mock.invocationCallOrder[0],
    );
    expect(editor).toBeInTheDocument();
    expect(editor).toHaveValue('دەقی پارێزراو');
    expect(
      screen.getAllByText(
        'The save result is uncertain after two exact attempts. All further decisions are blocked. Restart Cortex to reopen from database truth; your draft is retained.',
      ).length,
    ).toBeGreaterThan(0);
    expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(true);
    expect(screen.queryByText(/response lost/)).not.toBeInTheDocument();
    const audio = document.querySelector('audio')!;
    const exactTime = audio.currentTime;
    const authorityCalls = api.beginDesktopPlaybackSessionV1.mock.calls.length;
    const play = screen.getByRole('button', { name: en['audio.play'] });
    const seek = screen.getByRole('slider', { name: en['audio.seek'] });
    expect(play).toBeDisabled();
    expect(seek).toBeDisabled();
    await fireEvent.click(play);
    await fireEvent.keyDown(window, { key: ' ', code: 'Space' });
    await fireEvent.keyDown(window, { key: 'r', code: 'KeyR' });
    expect(audio.paused).toBe(true);
    expect(audio.currentTime).toBe(exactTime);
    expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalledTimes(authorityCalls);
    expect(editor).toHaveValue('دەقی پارێزراو');
  });

  it('hard-stops after a lost commit response and never risks a second truth write', async () => {
    const row = inboxSegment('lost-response', true);
    api.getReviewPageV1.mockResolvedValue(reviewPage([row]));
    api.commitReviewV1.mockRejectedValueOnce(new Error('response lost after durable commit'));

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await hearCurrentAudio();
    const accept = screen.getByRole('button', { name: /^A Accept$/ });
    await fireEvent.click(accept);
    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledTimes(1));
    const first = api.commitReviewV1.mock.calls[0][0];
    await waitFor(() => expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(true));
    expect(accept).toBeDisabled();
    await fireEvent.click(accept);
    expect(api.commitReviewV1).toHaveBeenCalledTimes(1);
    expect(first).toMatchObject({
      operationId: expect.any(String),
      playbackReceiptId: 'receipt-lost-response',
    });
    expect(api.recordPlaybackReceipt).toHaveBeenCalledOnce();
    expect(screen.queryByText('Accepted')).not.toBeInTheDocument();
  });

  it('hard-stops a two-attempt commit timeout without losing the exact clip, draft, or receipt', async () => {
    const onClose = vi.fn();
    const first = inboxSegment('commit-timeout-first', true);
    const second = inboxSegment('commit-timeout-second', true);
    const firstNativeAttempt = deferred<{
      segmentId: string;
      committedRevision: number;
      authoritativeTranscript: string;
      decisionId: string;
    }>();
    const secondNativeAttempt = deferred<{
      segmentId: string;
      committedRevision: number;
      authoritativeTranscript: string;
      decisionId: string;
    }>();
    const nativeRequests: unknown[] = [];
    api.getReviewPageV1.mockResolvedValue(reviewPage([first, second]));
    api.commitReviewV1.mockImplementation(async (request) => {
      const invokeExact = () => {
        nativeRequests.push(structuredClone(request));
        const source = nativeRequests.length === 1 ? firstNativeAttempt : secondNativeAttempt;
        return withReviewOperationTimeout(
          source.promise,
          'E_REVIEW_COMMIT_TIMEOUT',
          REVIEW_OPERATION_TIMEOUT_MS,
        );
      };
      try {
        return await invokeExact();
      } catch (error) {
        if (error instanceof Error) return invokeExact();
        throw error;
      }
    });

    render(ReviewInbox, { onClose });
    await screen.findByText('Queue (2)');
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    const editor = screen.getByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'دەقی timeout پارێزراو' } });
    const firstOption = screen.getByRole('option', { name: /commit-timeout-first/ });
    const secondOption = screen.getByRole('option', { name: /commit-timeout-second/ });

    vi.useFakeTimers();
    try {
      await fireEvent.click(screen.getByRole('button', { name: /Save edit/ }));
      await vi.advanceTimersByTimeAsync(0);
      expect(api.commitReviewV1).toHaveBeenCalledOnce();
      expect(nativeRequests).toHaveLength(1);
      expect(sharedDurableReviewUndo.state.truthWriteInFlight).toBe(true);

      await fireEvent.click(screen.getByRole('button', { name: 'Run Jury' }));
      await fireEvent.click(screen.getByRole('button', { name: /^F Flag$/ }));
      await fireEvent.click(screen.getByRole('button', { name: /Undo/ }));
      await fireEvent.click(screen.getByRole('button', { name: 'Close inbox' }));
      await fireEvent.click(secondOption);
      expect(api.getSegmentIdsForView).not.toHaveBeenCalled();
      expect(api.runJuryPipeline).not.toHaveBeenCalled();
      expect(api.recordReviewFlag).not.toHaveBeenCalled();
      expect(api.undoDesktopReviewActionV1).not.toHaveBeenCalled();
      expect(onClose).not.toHaveBeenCalled();
      expect(firstOption).toHaveAttribute('aria-selected', 'true');

      await vi.advanceTimersByTimeAsync(REVIEW_OPERATION_TIMEOUT_MS);
      expect(nativeRequests).toHaveLength(2);
      expect(nativeRequests[1]).toEqual(nativeRequests[0]);
      await vi.advanceTimersByTimeAsync(REVIEW_OPERATION_TIMEOUT_MS);
      expect(sharedDurableReviewUndo.state.truthWriteInFlight).toBe(false);
      expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(true);
      expect(sharedDurableReviewUndo.state.errorCode).toBe('E_REVIEW_COMMIT_TIMEOUT');
    } finally {
      vi.useRealTimers();
    }

    expect(editor).toBeInTheDocument();
    expect(editor).toHaveValue('دەقی timeout پارێزراو');
    expect(editor).toBeDisabled();
    expect(firstOption).toHaveAttribute('aria-selected', 'true');
    expect(secondOption).toHaveAttribute('aria-selected', 'false');
    expect(api.saveReviewDraftV1).toHaveBeenCalledWith(first.id, 7, 'دەقی timeout پارێزراو');
    expect(api.deleteReviewDraftV1).not.toHaveBeenCalled();
    expect(api.recordPlaybackReceipt).toHaveBeenCalledOnce();
    expect(api.recordPlaybackReceipt).toHaveBeenCalledWith({
      playbackReceiptId: `receipt-${first.id}`,
      mediaGrantId: MEDIA_GRANT_ID,
      intervals: [{ startMs: 0, endMs: 900 }],
    });
    expect(nativeRequests[0]).toMatchObject({
      operationId: expect.stringMatching(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
      ),
      segmentId: first.id,
      baseRevision: 7,
      decision: 'edit',
      transcript: 'دەقی timeout پارێزراو',
      playbackReceiptId: `receipt-${first.id}`,
    });
    expect(api.getReviewPageV1).toHaveBeenCalledOnce();
    expect(
      screen.getAllByText(
        'The save result is uncertain after two exact attempts. All further decisions are blocked. Restart Cortex to reopen from database truth; your draft is retained.',
      ).length,
    ).toBeGreaterThan(0);

    firstNativeAttempt.resolve({
      segmentId: first.id,
      committedRevision: 8,
      authoritativeTranscript: 'late first result',
      decisionId: 'effect:701',
    });
    secondNativeAttempt.resolve({
      segmentId: first.id,
      committedRevision: 8,
      authoritativeTranscript: 'late second result',
      decisionId: 'effect:702',
    });
    await Promise.resolve();
    await Promise.resolve();
    expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(true);
    expect(sharedDurableReviewUndo.state.errorCode).toBe('E_REVIEW_COMMIT_TIMEOUT');
    expect(api.getReviewPageV1).toHaveBeenCalledOnce();
    expect(screen.queryByText('Accepted')).not.toBeInTheDocument();
  });

  it('mints a new operation but reuses unconsumed playback after a proven UUID conflict', async () => {
    const row = inboxSegment('operation-conflict', true);
    api.getReviewPageV1.mockResolvedValue(reviewPage([row]));
    api.commitReviewV1.mockRejectedValueOnce({
      schema: 1,
      code: 'OPERATION_ID_CONFLICT',
      message: 'the operation UUID belongs to another payload',
      retryable: false,
    });

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await hearCurrentAudio();
    const accept = screen.getByRole('button', { name: /^A Accept$/ });
    await fireEvent.click(accept);
    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledTimes(1));
    const first = api.commitReviewV1.mock.calls[0][0];

    await waitFor(() => expect(accept).not.toBeDisabled());
    await fireEvent.click(accept);
    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledTimes(2));
    const second = api.commitReviewV1.mock.calls[1][0];
    expect(second.operationId).not.toBe(first.operationId);
    expect(second.playbackReceiptId).toBe(first.playbackReceiptId);
    expect(api.recordPlaybackReceipt).toHaveBeenCalledOnce();
    expect(await screen.findByText('Accepted')).toBeInTheDocument();
  });

  it('retires a definitively refused commit and succeeds only with fresh playback and operation identities', async () => {
    const row = inboxSegment('playback-refused', true);
    api.getReviewPageV1.mockResolvedValue(reviewPage([row]));
    let issuance = 0;
    api.beginDesktopPlaybackSessionV1.mockImplementation(
      async (segmentId: string, _mediaGrantId: string, expectedRevision: number) => ({
        playbackReceiptId: `receipt-attempt-${++issuance}`,
        segmentId,
        segmentRevision: expectedRevision,
        clipDurationMs: 1_000,
        expiresAtMs: Date.now() + 60_000,
      }),
    );
    api.recordPlaybackReceipt.mockImplementation(
      async ({ playbackReceiptId }: { playbackReceiptId: string }) => ({
        playbackReceiptId,
        segmentId: row.id,
        segmentRevision: 7,
        uniquePlayedMs: 900,
        clipDurationMs: 1_000,
        coverageRatio: 0.9,
      }),
    );
    api.commitReviewV1.mockRejectedValueOnce({
      schema: 1,
      code: 'NO_PLAYBACK_EVIDENCE',
      message: 'private backend detail',
      retryable: true,
    });

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^A Accept$/ }));

    expect(await screen.findByText(/Not saved: play the whole clip first/)).toBeInTheDocument();
    expect(
      screen.getByRole('option', { name: 'Segment 1 of 1: playback-refused' }),
    ).toBeInTheDocument();
    expect(screen.queryByText('private backend detail')).not.toBeInTheDocument();
    expect(api.recordPlaybackReceipt).toHaveBeenCalledOnce();
    const first = api.commitReviewV1.mock.calls[0][0];

    await waitFor(() => expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalledTimes(2));
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^A Accept$/ }));
    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledTimes(2));
    const second = api.commitReviewV1.mock.calls[1][0];
    expect(second.operationId).not.toBe(first.operationId);
    expect(second.playbackReceiptId).not.toBe(first.playbackReceiptId);
    expect(api.recordPlaybackReceipt).toHaveBeenCalledTimes(2);
    expect(await screen.findByText('Accepted')).toBeInTheDocument();
  });

  it('rejects a mismatched commit response and hard-stops without creating Undo truth', async () => {
    const row = inboxSegment('identity-bound', true);
    api.getReviewPageV1.mockResolvedValue(reviewPage([row]));
    api.commitReviewV1.mockResolvedValueOnce({
      segmentId: 'different-segment',
      committedRevision: 8,
      authoritativeTranscript: row.rawTranscript,
      decisionId: 'effect:404',
    });

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^A Accept$/ }));

    await waitFor(() => expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(true));
    expect(
      await screen.findByRole('option', { name: 'Segment 1 of 1: identity-bound' }),
    ).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^A Accept$/ })).toBeDisabled();
    expect(api.getReviewPageV1).toHaveBeenCalledTimes(1);
    expect(api.undoDesktopReviewActionV1).not.toHaveBeenCalled();
  });

  it('keeps a committed decision bound to its row and refuses a Jury interleave', async () => {
    const first = inboxSegment('aaaaaaaa-1', true);
    const reloaded = inboxSegment('cccccccc-3');
    api.getReviewPageV1
      .mockResolvedValueOnce(reviewPage([first, inboxSegment('bbbbbbbb-2')]))
      .mockResolvedValueOnce(reviewPage([reloaded]))
      .mockResolvedValue(reviewPage([reloaded]));
    let commitDecision!: (value: {
      segmentId: string;
      committedRevision: number;
      authoritativeTranscript: string;
      decisionId: string;
    }) => void;
    api.commitReviewV1.mockReturnValue(
      new Promise((resolve) => {
        commitDecision = resolve;
      }),
    );
    render(ReviewInbox);
    await screen.findByText('Queue (2)');
    await waitFor(() => expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalled());
    await hearCurrentAudio();

    await fireEvent.click(screen.getByRole('button', { name: /Accept/ }));
    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalled());
    expect(api.recordPlaybackReceipt).toHaveBeenCalledWith({
      playbackReceiptId: 'receipt-aaaaaaaa-1',
      mediaGrantId: MEDIA_GRANT_ID,
      intervals: [{ startMs: 0, endMs: 900 }],
    });
    expect(api.commitReviewV1).toHaveBeenCalledWith({
      operationId: expect.any(String),
      segmentId: 'aaaaaaaa-1',
      baseRevision: 7,
      decision: 'accept',
      transcript: 'دەق',
      reasonCode: null,
      playbackReceiptId: 'receipt-aaaaaaaa-1',
    });

    // Jury is itself a truth writer. It cannot even discover a target set while the human decision
    // owns the shared lease, so there is no competing reload or batch mutation to reconcile later.
    await fireEvent.click(screen.getByRole('button', { name: /Run Jury/ }));
    expect(screen.getByText('Queue (2)')).toBeInTheDocument();
    expect(api.getSegmentIdsForView).not.toHaveBeenCalled();
    expect(api.runJuryPipeline).not.toHaveBeenCalled();
    await waitFor(() =>
      expect(
        screen
          .getAllByRole('status')
          .some(
            (status) =>
              status.textContent === 'A review change is still being saved. Wait for it to finish.',
          ),
      ).toBe(true),
    );
    expect(screen.getByText('Queue (2)')).toBeInTheDocument();

    commitDecision({
      segmentId: first.id,
      committedRevision: 8,
      authoritativeTranscript: first.rawTranscript,
      decisionId: 'effect:7',
    });

    await waitFor(() => expect(screen.getByText('Accepted')).toBeInTheDocument());
    // The decision's own mandatory projection reconciliation may now install the shorter page.
    expect(screen.getByText('Queue (1)')).toBeInTheDocument();
    expect(screen.getByText('cccccccc…')).toBeInTheDocument();
    expect(screen.queryByText('aaaaaaaa…')).not.toBeInTheDocument();
  });

  it('aborts a correction commit when receipt finalization fails and preserves the typed draft', async () => {
    api.getReviewPageV1.mockResolvedValue(reviewPage([inboxSegment('aaaaaaaa-1', true)]));
    api.recordPlaybackReceipt.mockRejectedValue(new Error('receipt store unavailable'));

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await waitFor(() => expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalled());
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    const editor = screen.getByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'دەقی ڕاستکراوە' } });

    await fireEvent.click(screen.getByRole('button', { name: /Save edit/ }));

    await waitFor(() => expect(api.recordPlaybackReceipt).toHaveBeenCalledTimes(1));
    expect(api.commitReviewV1).not.toHaveBeenCalled();
    expect(editor).toHaveValue('دەقی ڕاستکراوە');
    expect(editor).toBeInTheDocument();
    expect(
      screen.getByText('Failed to save edit: An unknown review error occurred.'),
    ).toBeInTheDocument();
    expect(screen.queryByText(/receipt store unavailable/)).not.toBeInTheDocument();
  });

  it('keeps Reconcile enabled after a confirmed write whose projection reload failed', async () => {
    const first = inboxSegment('reconcile-first', true);
    const next = inboxSegment('reconcile-next', true);
    api.getReviewPageV1
      .mockResolvedValueOnce(reviewPage([first]))
      .mockResolvedValueOnce(reviewPage([next]))
      .mockResolvedValueOnce(reviewPage([next]));
    api.getSegmentsPage
      .mockRejectedValueOnce(new Error('global projection unavailable'))
      .mockResolvedValueOnce({ items: [], total: 0, nextCursor: null, revisions: {} });

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await waitFor(() => expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalled());
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^A Accept$/ }));

    await waitFor(() => expect(sharedDurableReviewUndo.state.truthProjectionPending).toBe(true));
    await waitFor(() => expect(sharedDurableReviewUndo.state.status).toBe('failed'));
    expect(screen.getByText('reconcile-next.wav')).toBeInTheDocument();
    const reconcile = await screen.findByRole('button', { name: 'Reconcile saved decision' });
    await waitFor(() => expect(reconcile).toBeEnabled());
    expect(reconcile).not.toHaveAttribute('aria-describedby');

    await fireEvent.click(reconcile);
    await waitFor(() => expect(sharedDurableReviewUndo.state.truthProjectionPending).toBe(false));
    expect(sharedDurableReviewUndo.state.status).toBe('ready');
    expect(api.getSegmentsPage).toHaveBeenCalledTimes(2);
    expect(api.getReviewPageV1).toHaveBeenCalledTimes(3);
    expect(api.undoDesktopReviewActionV1).not.toHaveBeenCalled();
    expect(
      screen.getByText(
        'The saved decision and every open view are now reconciled with database truth.',
      ),
    ).toBeInTheDocument();
  });

  it('releases a timed-out playback barrier and retries the exact immutable receipt before writing truth', async () => {
    const onClose = vi.fn();
    const first = inboxSegment('playback-timeout-first', true);
    const second = inboxSegment('playback-timeout-second', true);
    const firstFinalization = deferred<{
      playbackReceiptId: string;
      segmentId: string;
      segmentRevision: number;
      uniquePlayedMs: number;
      clipDurationMs: number;
      coverageRatio: number;
    }>();
    api.getReviewPageV1.mockResolvedValue(reviewPage([first, second]));
    api.recordPlaybackReceipt
      .mockImplementationOnce((_request) =>
        withReviewOperationTimeout(
          firstFinalization.promise,
          'E_PLAYBACK_FINALIZATION_TIMEOUT',
          REVIEW_OPERATION_TIMEOUT_MS,
        ),
      )
      .mockImplementationOnce(async ({ playbackReceiptId }) => ({
        playbackReceiptId,
        segmentId: first.id,
        segmentRevision: 7,
        uniquePlayedMs: 900,
        clipDurationMs: 1_000,
        coverageRatio: 0.9,
      }));

    render(ReviewInbox, { onClose });
    await screen.findByText('Queue (2)');
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    const editor = screen.getByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'دەقی receipt پارێزراو' } });
    const firstOption = screen.getByRole('option', { name: /playback-timeout-first/ });
    const secondOption = screen.getByRole('option', { name: /playback-timeout-second/ });

    vi.useFakeTimers();
    try {
      await fireEvent.click(screen.getByRole('button', { name: /Save edit/ }));
      await vi.advanceTimersByTimeAsync(0);
      expect(api.recordPlaybackReceipt).toHaveBeenCalledOnce();
      expect(api.commitReviewV1).not.toHaveBeenCalled();
      expect(sharedDurableReviewUndo.state.truthWriteInFlight).toBe(true);

      await fireEvent.click(screen.getByRole('button', { name: 'Run Jury' }));
      await fireEvent.click(screen.getByRole('button', { name: /^F Flag$/ }));
      await fireEvent.click(screen.getByRole('button', { name: /Undo/ }));
      await fireEvent.click(screen.getByRole('button', { name: 'Close inbox' }));
      await fireEvent.click(secondOption);
      expect(api.getSegmentIdsForView).not.toHaveBeenCalled();
      expect(api.runJuryPipeline).not.toHaveBeenCalled();
      expect(api.recordReviewFlag).not.toHaveBeenCalled();
      expect(api.undoDesktopReviewActionV1).not.toHaveBeenCalled();
      expect(onClose).not.toHaveBeenCalled();
      expect(firstOption).toHaveAttribute('aria-selected', 'true');

      await vi.advanceTimersByTimeAsync(REVIEW_OPERATION_TIMEOUT_MS);
      expect(sharedDurableReviewUndo.state.truthWriteInFlight).toBe(false);
      expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(false);
      expect(sharedDurableReviewUndo.state.errorCode).toBeNull();
    } finally {
      vi.useRealTimers();
    }

    const exactAttempt = api.recordPlaybackReceipt.mock.calls[0][0];
    expect(exactAttempt).toEqual({
      playbackReceiptId: `receipt-${first.id}`,
      mediaGrantId: MEDIA_GRANT_ID,
      intervals: [{ startMs: 0, endMs: 900 }],
    });
    expect(editor).toBeInTheDocument();
    expect(editor).toHaveValue('دەقی receipt پارێزراو');
    expect(editor).toBeEnabled();
    expect(firstOption).toHaveAttribute('aria-selected', 'true');
    expect(secondOption).toHaveAttribute('aria-selected', 'false');
    expect(screen.getByRole('button', { name: /Save edit/ })).toBeEnabled();
    expect(screen.getByText('Failed to save edit: E_PLAYBACK_FINALIZATION_TIMEOUT')).toBeVisible();
    expect(api.deleteReviewDraftV1).not.toHaveBeenCalled();
    expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalledOnce();

    await fireEvent.click(screen.getByRole('button', { name: /Save edit/ }));
    await waitFor(() => expect(api.recordPlaybackReceipt).toHaveBeenCalledTimes(2));
    expect(api.recordPlaybackReceipt.mock.calls[1][0]).toEqual(exactAttempt);
    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledOnce());
    expect(api.commitReviewV1).toHaveBeenCalledWith(
      expect.objectContaining({
        segmentId: first.id,
        baseRevision: 7,
        decision: 'edit',
        transcript: 'دەقی receipt پارێزراو',
        playbackReceiptId: `receipt-${first.id}`,
      }),
    );
    expect(await screen.findByText('Edited')).toBeInTheDocument();

    firstFinalization.resolve({
      playbackReceiptId: `receipt-${first.id}`,
      segmentId: first.id,
      segmentRevision: 7,
      uniquePlayedMs: 900,
      clipDurationMs: 1_000,
      coverageRatio: 0.9,
    });
    await Promise.resolve();
    await Promise.resolve();
    expect(api.recordPlaybackReceipt).toHaveBeenCalledTimes(2);
    expect(api.commitReviewV1).toHaveBeenCalledOnce();
    expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(false);
    expect(screen.getByText('Edited')).toBeInTheDocument();
  });

  it('reissues same-segment authority after a stale revision and never reuses the old receipt', async () => {
    const row = inboxSegment('aaaaaaaa-1', true);
    api.getReviewPageV1
      .mockResolvedValueOnce(reviewPage([row], { baseRevision: 7 }))
      .mockResolvedValueOnce(reviewPage([row], { baseRevision: 8 }));
    api.beginDesktopPlaybackSessionV1.mockImplementation(
      async (segmentId: string, _grant: string, expectedRevision: number) => ({
        playbackReceiptId: `receipt-${segmentId}-revision-${expectedRevision}`,
        segmentId,
        segmentRevision: expectedRevision,
        clipDurationMs: 1_000,
        expiresAtMs: Date.now() + 60_000,
      }),
    );
    api.recordPlaybackReceipt.mockImplementation(async ({ playbackReceiptId }) => ({
      playbackReceiptId,
      segmentId: row.id,
      segmentRevision: Number(playbackReceiptId.replace(/^.*-revision-/, '')),
      uniquePlayedMs: 900,
      clipDurationMs: 1_000,
      coverageRatio: 0.9,
    }));
    api.commitReviewV1
      .mockRejectedValueOnce({
        schema: 1,
        code: 'STALE_REVISION',
        message: 'row changed',
        retryable: false,
      })
      .mockImplementationOnce(async (request) => ({
        segmentId: request.segmentId,
        committedRevision: request.baseRevision + 1,
        authoritativeTranscript: request.transcript ?? row.rawTranscript,
        decisionId: 'effect:808',
      }));

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await waitFor(() =>
      expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalledWith(
        row.id,
        MEDIA_GRANT_ID,
        7,
        expect.any(String),
      ),
    );
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^A Accept$/ }));

    await waitFor(() =>
      expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalledWith(
        row.id,
        MEDIA_GRANT_ID,
        8,
        expect.any(String),
      ),
    );
    const issuanceCalls = api.beginDesktopPlaybackSessionV1.mock.calls;
    expect(issuanceCalls[1][3]).not.toBe(issuanceCalls[0][3]);
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^A Accept$/ }));

    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledTimes(2));
    expect(api.recordPlaybackReceipt.mock.calls[1][0]).toMatchObject({
      playbackReceiptId: 'receipt-aaaaaaaa-1-revision-8',
    });
    expect(api.commitReviewV1.mock.calls[1][0]).toMatchObject({
      baseRevision: 8,
      playbackReceiptId: 'receipt-aaaaaaaa-1-revision-8',
    });
  });

  it('uses the typed revision-bound commit for a correction', async () => {
    api.getReviewPageV1.mockResolvedValue(
      reviewPage([inboxSegment('aaaaaaaa-1', true)], { baseRevision: 7 }),
    );

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await waitFor(() => expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalled());
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    await fireEvent.input(screen.getByRole('textbox'), {
      target: { value: 'دەقی ڕاستکراوە' },
    });
    await fireEvent.click(screen.getByRole('button', { name: /Save edit/ }));

    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledTimes(1));
    expect(api.commitReviewV1).toHaveBeenCalledWith({
      operationId: expect.any(String),
      segmentId: 'aaaaaaaa-1',
      baseRevision: 7,
      decision: 'edit',
      transcript: 'دەقی ڕاستکراوە',
      reasonCode: null,
      playbackReceiptId: 'receipt-aaaaaaaa-1',
    });
  });

  it('uses the typed revision-bound commit for rejection', async () => {
    api.getReviewPageV1.mockResolvedValue(
      reviewPage([inboxSegment('aaaaaaaa-1', true)], { baseRevision: 7 }),
    );

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await waitFor(() => expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalled());
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^X Reject$/ }));

    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledTimes(1));
    expect(api.commitReviewV1).toHaveBeenCalledWith({
      operationId: expect.any(String),
      segmentId: 'aaaaaaaa-1',
      baseRevision: 7,
      decision: 'reject',
      transcript: null,
      reasonCode: null,
      playbackReceiptId: 'receipt-aaaaaaaa-1',
    });
  });

  it('treats a typed conflict as terminal and reloads every authoritative projection', async () => {
    const row = inboxSegment('aaaaaaaa-1', true);
    api.getReviewPageV1.mockResolvedValue(reviewPage([row]));
    api.undoDesktopReviewActionV1.mockImplementationOnce(async (target: UndoTarget) => {
      undoAvailability = { status: 'blocked', reason: 'decisionShadowed' };
      return { status: 'conflict', effectKind: target.kind, effectEventId: target.effectEventId };
    });

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^A Accept$/ }));
    expect(await screen.findByText('Accepted')).toBeInTheDocument();

    const undo = screen.getByRole('button', { name: /Undo/ });
    await fireEvent.click(undo);
    await waitFor(() => expect(api.undoDesktopReviewActionV1).toHaveBeenCalledTimes(1));
    const [target, operationId] = api.undoDesktopReviewActionV1.mock.calls[0];
    expect(target).toEqual(
      undoTarget(row.id, 'accept', target.sourceOperationId, target.effectEventId),
    );
    expect(operationId).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
    expect(await screen.findByText(/Failed to undo:/)).toHaveTextContent(
      'The segment changed after this decision',
    );
    expect(api.getReviewPageV1).toHaveBeenCalledTimes(3);
    expect(api.getSegmentsPage).toHaveBeenCalled();
    await fireEvent.click(undo);
    expect(api.undoDesktopReviewActionV1).toHaveBeenCalledTimes(1);
  });

  it('offers the latest generic flag through the shared Backspace Undo flow', async () => {
    const row = inboxSegment('aaaaaaaa-1');
    api.getReviewPageV1.mockResolvedValue(reviewPage([row]));

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await fireEvent.click(screen.getByRole('button', { name: /^F Flag$/ }));
    expect(await screen.findByText('Flagged for second pass')).toBeInTheDocument();
    const undo = screen.getByRole('button', { name: /Undo/ });
    expect(undo).toBeEnabled();
    await fireEvent.keyDown(window, { key: 'Backspace', code: 'Backspace' });
    await waitFor(() => expect(api.undoDesktopReviewActionV1).toHaveBeenCalledOnce());
    const [target] = api.undoDesktopReviewActionV1.mock.calls[0];
    expect(target).toMatchObject({
      kind: 'flag',
      segmentId: row.id,
      effectEventId: 202,
      flagKind: { kind: 'generic' },
    });
    await waitFor(() =>
      expect(sharedDurableReviewUndo.state.blockedReason).toBe('latestFlagUndone'),
    );
  });

  it('hard-stops a cross-table flag Undo response without advancing projections', async () => {
    const row = inboxSegment('malformed-flag-undo');
    api.getReviewPageV1.mockResolvedValue(reviewPage([row]));
    api.undoDesktopReviewActionV1.mockImplementationOnce(async (target: UndoTarget) => ({
      status: 'alreadyApplied',
      effectKind: 'decision',
      effectEventId: target.effectEventId,
    }));

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await fireEvent.click(screen.getByRole('button', { name: /^F Flag$/ }));
    expect(await screen.findByText('Flagged for second pass')).toBeInTheDocument();
    const projectionReadsBeforeUndo = api.getReviewPageV1.mock.calls.length;
    await fireEvent.keyDown(window, { key: 'Backspace', code: 'Backspace' });

    await waitFor(() => expect(api.undoDesktopReviewActionV1).toHaveBeenCalledOnce());
    expect(sharedDurableReviewUndo.state).toMatchObject({
      status: 'reconciling',
      errorCode: 'INVALID_UNDO_RESPONSE',
      inFlight: false,
    });
    expect(sharedDurableReviewUndo.blocksNewTruth()).toBe(true);
    expect(api.getReviewPageV1).toHaveBeenCalledTimes(projectionReadsBeforeUndo);
  });

  it('sends rendered r5 authority and reconciles a typed stale refusal after server mutation r6', async () => {
    const renderedR5 = inboxSegment('flag-revision-cas');
    const serverR6 = {
      ...renderedR5,
      normalizedTranscript: 'server truth at r6',
    };
    api.getReviewPageV1
      .mockResolvedValueOnce(reviewPage([renderedR5], { baseRevision: 5 }))
      .mockResolvedValueOnce(reviewPage([serverR6], { baseRevision: 6 }))
      .mockResolvedValueOnce(reviewPage([], { baseRevision: 7 }));
    api.recordReviewFlag.mockRejectedValueOnce({
      schema: 1,
      code: 'STALE_REVISION',
      message: 'This clip changed; reload it before flagging it.',
      retryable: false,
      suggestedAction: 'reloadClip',
      operationId: 'server-echoes-the-real-operation-id',
      details: { expectedRevision: 5, currentRevision: 6 },
    });

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await fireEvent.click(screen.getByRole('button', { name: /^F Flag$/ }));

    await waitFor(() => expect(api.recordReviewFlag).toHaveBeenCalledTimes(1));
    expect(api.recordReviewFlag).toHaveBeenCalledWith({
      operationId: expect.stringMatching(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
      ),
      segmentId: renderedR5.id,
      baseRevision: 5,
      rationale: 'Flagged for second-pass adjudication',
    });
    await waitFor(() => expect(api.getReviewPageV1).toHaveBeenCalledTimes(2));
    expect(screen.queryByText('Flagged for second pass')).not.toBeInTheDocument();
    expect(screen.getByText('Failed to flag: STALE_REVISION')).toBeInTheDocument();
    expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(false);
    expect(sharedDurableReviewUndo.state.status).not.toBe('blocked');

    const firstRequest = api.recordReviewFlag.mock.calls[0][0];
    await fireEvent.click(screen.getByRole('button', { name: /^F Flag$/ }));
    await waitFor(() => expect(api.recordReviewFlag).toHaveBeenCalledTimes(2));
    const secondRequest = api.recordReviewFlag.mock.calls[1][0];
    expect(secondRequest).toMatchObject({
      segmentId: renderedR5.id,
      baseRevision: 6,
      rationale: firstRequest.rationale,
    });
    expect(secondRequest.operationId).not.toBe(firstRequest.operationId);
    await waitFor(() => expect(api.getReviewPageV1).toHaveBeenCalledTimes(3));
  });

  it('retains an exact latest-flag target across an authoritative availability refresh', async () => {
    const row = inboxSegment('flag-conflict');
    api.getReviewPageV1.mockResolvedValue(reviewPage([row]));

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await fireEvent.click(screen.getByRole('button', { name: /^F Flag$/ }));
    expect(await screen.findByText('Flagged for second pass')).toBeInTheDocument();

    const undo = screen.getByRole('button', { name: /Undo/ });
    expect(undo).toBeEnabled();
    const before = sharedDurableReviewUndo.state.target;
    await sharedDurableReviewUndo.refresh();
    expect(sharedDurableReviewUndo.state.status).toBe('ready');
    expect(sharedDurableReviewUndo.state.target).toEqual(before);
    expect(undo).toBeEnabled();
  });

  it('releases a pre-writer Undo draft timeout and retries the same click-time operation safely', async () => {
    const row = inboxSegment('undo-draft-timeout');
    const target = undoTarget(row.id, 'edit', '0f4a27cc-3255-4b3e-bf48-f4a1444d4a33', 606);
    const durableUndo = createDurableReviewUndoController();
    Object.assign(durableUndo.state, {
      status: 'ready',
      target,
      operationId: null,
      blockedReason: null,
      errorCode: null,
      inFlight: false,
      truthWriteInFlight: false,
      truthWriteAmbiguous: false,
      truthProjectionPending: false,
      projectionOutcome: null,
    });
    const draftBarrier = deferred<void>();
    const flush = vi
      .fn()
      .mockImplementationOnce(() =>
        withReviewOperationTimeout(
          draftBarrier.promise,
          'E_REVIEW_DRAFT_SAVE_TIMEOUT',
          REVIEW_OPERATION_TIMEOUT_MS,
        ),
      )
      .mockResolvedValueOnce(undefined);
    let queueEpoch = 1;
    const queue = {
      state: {
        rows: [row],
        revisions: { [row.id]: 7 },
        eligibility: { [row.id]: { eligible: true, disabledReason: null } },
      },
      current: () => row,
      currentRevision: () => 7,
      currentEligibility: () => ({ eligible: true, disabledReason: null }),
      canAdvance: () => false,
      reloadProjection: vi.fn(async () => ++queueEpoch),
      projectionReceipt: vi.fn(() => queueEpoch),
    };
    const draft = {
      state: {
        editing: false,
        editText: row.rawTranscript,
        editingForId: null,
        baseline: row.rawTranscript,
        readyId: row.id,
        conflict: null,
        recovered: false,
        saving: false,
        saveFailed: false,
        loadError: null,
        pending: false,
      },
      blockedKey: () => null,
      flush,
    };
    const status = vi.fn();
    const controller = createReviewInboxDecisionController(
      {
        queue,
        draft,
        playback: { state: { audioError: null } },
        setStatus: status,
      } as unknown as Parameters<typeof createReviewInboxDecisionController>[0],
      durableUndo,
    );

    vi.useFakeTimers();
    try {
      const firstUndo = controller.undo();
      await vi.advanceTimersByTimeAsync(0);
      expect(flush).toHaveBeenCalledOnce();
      expect(durableUndo.state.status).toBe('reconciling');
      expect(durableUndo.state.inFlight).toBe(true);
      expect(api.undoDesktopReviewActionV1).not.toHaveBeenCalled();

      await vi.advanceTimersByTimeAsync(REVIEW_OPERATION_TIMEOUT_MS);
      await firstUndo;
      expect(durableUndo.state.status).toBe('ready');
      expect(durableUndo.state.inFlight).toBe(false);
      expect(durableUndo.state.truthWriteAmbiguous).toBe(false);
      expect(durableUndo.state.target).toEqual(target);
      expect(durableUndo.state.operationId).toMatch(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
      );
      expect(controller.state.submitting).toBe(false);
      expect(controller.actionKeys().undo).toBeNull();
      expect(status).toHaveBeenLastCalledWith('Failed to undo: E_REVIEW_DRAFT_SAVE_TIMEOUT');
      expect(api.undoDesktopReviewActionV1).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }

    const retainedOperationId = durableUndo.state.operationId;
    draftBarrier.resolve();
    await Promise.resolve();
    expect(durableUndo.state.status).toBe('ready');
    expect(durableUndo.state.operationId).toBe(retainedOperationId);
    expect(api.undoDesktopReviewActionV1).not.toHaveBeenCalled();

    undoAvailability = { status: 'none' };
    api.undoDesktopReviewActionV1.mockResolvedValueOnce({
      status: 'conflict',
      effectKind: target.kind,
      effectEventId: target.effectEventId,
    });
    await controller.undo();
    expect(flush).toHaveBeenCalledTimes(2);
    expect(api.undoDesktopReviewActionV1).toHaveBeenCalledOnce();
    expect(api.undoDesktopReviewActionV1).toHaveBeenCalledWith(target, retainedOperationId);
    expect(durableUndo.state.truthWriteAmbiguous).toBe(false);
    expect(durableUndo.state.status).toBe('none');
    controller.disposeUndoProjection();
  });

  it('retries an ambiguous undo response with the exact same inverse operation', async () => {
    const row = inboxSegment('undo-response', true);
    api.getReviewPageV1.mockResolvedValue(reviewPage([row]));
    api.undoDesktopReviewActionV1
      .mockRejectedValueOnce(new Error('response lost after durable inverse'))
      .mockImplementationOnce(async (target: UndoTarget) => {
        undoAvailability = { status: 'blocked', reason: 'latestDecisionUndone' };
        return {
          status: 'applied',
          effectKind: target.kind,
          effectEventId: target.effectEventId,
          restoredRevision: 9,
          segment: row,
        };
      });

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^A Accept$/ }));
    expect(await screen.findByText('Accepted')).toBeInTheDocument();

    const undo = screen.getByRole('button', { name: /Undo/ });
    await fireEvent.click(undo);
    await waitFor(() => expect(api.undoDesktopReviewActionV1).toHaveBeenCalledTimes(1));
    const first = api.undoDesktopReviewActionV1.mock.calls[0];
    expect(
      await screen.findByText(
        'The Undo result is uncertain. New decisions are blocked; use Undo again to reconcile the same operation.',
      ),
    ).toBeInTheDocument();

    await fireEvent.click(undo);
    await waitFor(() => expect(api.undoDesktopReviewActionV1).toHaveBeenCalledTimes(2));
    expect(api.undoDesktopReviewActionV1.mock.calls[1]).toEqual(first);
    expect(first[0]).toEqual(
      undoTarget(row.id, 'accept', first[0].sourceOperationId, first[0].effectEventId),
    );
    expect(first[1]).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
    expect(await screen.findByText('Undone')).toBeInTheDocument();
  });

  it('advances without publishing truth and makes the no-decision outcome explicit', async () => {
    const first = inboxSegment('skip-first');
    const second = inboxSegment('skip-second');
    api.getReviewPageV1.mockResolvedValue(reviewPage([first, second]));

    render(ReviewInbox);
    await screen.findByText('Queue (2)');
    await fireEvent.click(screen.getByRole('button', { name: /^S Next — no decision$/ }));

    await waitFor(() =>
      expect(screen.getByText('skip-sec…').closest('[role="option"]')).toHaveAttribute(
        'aria-selected',
        'true',
      ),
    );
    expect(await screen.findByText('No decision was recorded for this clip.')).toBeInTheDocument();
    expect(api.commitReviewV1).not.toHaveBeenCalled();
  });

  it('fails closed when the typed page says the segment is not eligible', async () => {
    api.getReviewPageV1.mockResolvedValue(
      reviewPage([inboxSegment('aaaaaaaa-1', true)], {
        eligible: false,
        disabledReason: 'TRANSCRIPT_NOT_READY',
      }),
    );
    render(ReviewInbox);
    await screen.findByText('Queue (1)');

    const accept = screen.getByRole('button', { name: /^A Accept$/ });
    expect(accept).toBeDisabled();
    expect(accept).toHaveAccessibleDescription(
      'This segment is not eligible for a review decision. Reload the queue after its transcript is ready.',
    );
    expect(api.recordPlaybackReceipt).not.toHaveBeenCalled();
    expect(api.commitReviewV1).not.toHaveBeenCalled();
  });

  it('retains the selected clip on screen while missing revision authority disables decisions', async () => {
    const row = { ...inboxSegment('missing-revision'), rawTranscript: 'visible server truth' };
    const page = reviewPage([row]);
    page.items[0].baseRevision = undefined as unknown as number;
    api.getReviewPageV1.mockResolvedValue(page);

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    expect(screen.getByText('visible server truth')).toBeInTheDocument();
    const accept = screen.getByRole('button', { name: /^A Accept$/ });
    expect(accept).toBeDisabled();
    expect(accept).toHaveAccessibleDescription(
      'This segment is not eligible for a review decision. Reload the queue after its transcript is ready.',
    );
    expect(api.commitReviewV1).not.toHaveBeenCalled();
  });

  it('keeps a typed correction authoritative and flushes it before rail navigation', async () => {
    api.getReviewPageV1.mockResolvedValue(
      reviewPage([inboxSegment('aaaaaaaa-1'), inboxSegment('bbbbbbbb-2')]),
    );
    render(ReviewInbox);
    await screen.findByText('Queue (2)');

    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    const editor = screen.getByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'دەقی ڕاستکراوە' } });

    const accept = screen.getByRole('button', { name: /^A Accept$/ });
    expect(accept).toBeDisabled();
    expect(accept).toHaveAttribute('aria-describedby', 'inbox-accept-disabled-reason');
    expect(accept).toHaveAccessibleDescription(
      'Save the correction or explicitly discard it before accepting.',
    );
    for (const name of [/^E Edit$/, /^X Reject$/, /^F Flag$/]) {
      const action = screen.getByRole('button', { name });
      expect(action).toBeDisabled();
      expect(action).toHaveAccessibleDescription(
        'Save or cancel the current correction before choosing another action.',
      );
    }
    const undo = screen.getByRole('button', { name: /Undo/ });
    expect(undo).toBeDisabled();
    expect(undo).toHaveAccessibleDescription(
      'There is no committed desktop review action to undo.',
    );
    await fireEvent.keyDown(window, { key: 'a', code: 'KeyA' });
    expect(api.commitReviewV1).not.toHaveBeenCalled();
    expect(editor).toHaveValue('دەقی ڕاستکراوە');

    const listbox = screen.getByRole('listbox', { name: 'Queue (2)' });
    const options = within(listbox).getAllByRole('option');
    await fireEvent.click(options[1]);
    await waitFor(() => expect(options[1]).toHaveAttribute('aria-selected', 'true'));
    expect(api.saveReviewDraftV1).toHaveBeenCalledWith('aaaaaaaa-1', 7, 'دەقی ڕاستکراوە');
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
  });

  it('refuses rail navigation when the outgoing draft cannot be stored', async () => {
    const first = inboxSegment('draft-owner');
    const second = inboxSegment('other-row');
    api.getReviewPageV1.mockResolvedValue(reviewPage([first, second]));
    api.saveReviewDraftV1.mockRejectedValue(new Error('disk full'));

    render(ReviewInbox);
    await screen.findByText('Queue (2)');
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    const editor = screen.getByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'unsaved correction' } });
    const options = within(screen.getByRole('listbox')).getAllByRole('option');
    await fireEvent.click(options[1]);

    expect(
      await screen.findByText('Close paused — the review draft is not safely stored'),
    ).toBeInTheDocument();
    expect(options[0]).toHaveAttribute('aria-selected', 'true');
    expect(options[1]).toHaveAttribute('aria-selected', 'false');
    await waitFor(() => expect(document.activeElement).toBe(editor));
    expect(editor).toHaveValue('unsaved correction');
  });

  it('refuses a late rail selection when truth authority begins during the draft flush', async () => {
    const first = inboxSegment('draft-owner');
    const second = inboxSegment('other-row');
    const pendingSave = deferred<{
      segmentId: string;
      baseRevision: number;
      text: string;
      updatedAt: string;
    }>();
    api.getReviewPageV1.mockResolvedValue(reviewPage([first, second]));
    api.saveReviewDraftV1.mockReturnValue(pendingSave.promise);

    render(ReviewInbox);
    await screen.findByText('Queue (2)');
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    const editor = screen.getByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'exact held correction' } });
    const options = within(screen.getByRole('listbox')).getAllByRole('option');
    await fireEvent.click(options[1]);
    await waitFor(() => expect(api.saveReviewDraftV1).toHaveBeenCalledOnce());

    Object.assign(sharedDurableReviewUndo.state, {
      status: 'none',
      truthWriteInFlight: true,
      truthWriteAmbiguous: false,
      truthProjectionPending: false,
    });
    pendingSave.resolve({
      segmentId: first.id,
      baseRevision: 7,
      text: 'exact held correction',
      updatedAt: '2026-08-28T00:00:00Z',
    });
    await pendingSave.promise;
    await Promise.resolve();

    expect(options[0]).toHaveAttribute('aria-selected', 'true');
    expect(options[1]).toHaveAttribute('aria-selected', 'false');
    expect(editor).toHaveValue('exact held correction');
  });

  it('lets only the latest identity-bound rail intent survive one held draft flush', async () => {
    const first = inboxSegment('first-row');
    const second = inboxSegment('second-row');
    const third = inboxSegment('third-row');
    const pendingSave = deferred<{
      segmentId: string;
      baseRevision: number;
      text: string;
      updatedAt: string;
    }>();
    api.getReviewPageV1.mockResolvedValue(reviewPage([first, second, third]));
    api.saveReviewDraftV1.mockReturnValue(pendingSave.promise);

    render(ReviewInbox);
    await screen.findByText('Queue (3)');
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    await fireEvent.input(screen.getByRole('textbox'), { target: { value: 'held draft' } });
    const options = within(screen.getByRole('listbox')).getAllByRole('option');
    await fireEvent.click(options[1]);
    await fireEvent.click(options[2]);
    await waitFor(() => expect(api.saveReviewDraftV1).toHaveBeenCalledOnce());

    pendingSave.resolve({
      segmentId: first.id,
      baseRevision: 7,
      text: 'held draft',
      updatedAt: '2026-08-28T00:00:00Z',
    });
    await waitFor(() => expect(options[2]).toHaveAttribute('aria-selected', 'true'));
    expect(options[0]).toHaveAttribute('aria-selected', 'false');
    expect(options[1]).toHaveAttribute('aria-selected', 'false');
  });

  it('skips across a page boundary only after the next cursor is durably loaded', async () => {
    const first = inboxSegment('page-one');
    const second = inboxSegment('page-two');
    api.getReviewPageV1
      .mockResolvedValueOnce(reviewPage([first], { total: 2, nextCursor: 'cursor-1' }))
      .mockResolvedValueOnce(reviewPage([second], { total: 2, nextCursor: null }));

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await fireEvent.click(screen.getByRole('button', { name: /^S Next — no decision$/ }));

    await waitFor(() => expect(api.getReviewPageV1).toHaveBeenCalledTimes(2));
    expect(api.getReviewPageV1.mock.calls[1][1]).toBe('cursor-1');
    await waitFor(() =>
      expect(screen.getByRole('option', { name: 'Segment 2 of 2: page-two' })).toHaveAttribute(
        'aria-selected',
        'true',
      ),
    );
    expect(api.commitReviewV1).not.toHaveBeenCalled();
  });

  it('does not merge or advance a page-boundary intent after truth authority starts', async () => {
    const first = inboxSegment('page-one');
    const second = inboxSegment('page-two');
    const latePage = deferred<ReturnType<typeof reviewPage>>();
    api.getReviewPageV1
      .mockResolvedValueOnce(reviewPage([first], { total: 2, nextCursor: 'cursor-1' }))
      .mockReturnValueOnce(latePage.promise);

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await fireEvent.click(screen.getByRole('button', { name: /^S Next — no decision$/ }));
    await waitFor(() => expect(api.getReviewPageV1).toHaveBeenCalledTimes(2));

    Object.assign(sharedDurableReviewUndo.state, {
      status: 'none',
      truthWriteInFlight: true,
      truthWriteAmbiguous: false,
      truthProjectionPending: false,
    });
    latePage.resolve(reviewPage([second], { total: 2, nextCursor: null }));
    await latePage.promise;
    await Promise.resolve();

    expect(screen.getByRole('option', { name: 'Segment 1 of 1: page-one' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    expect(screen.queryByText('page-two')).not.toBeInTheDocument();
    expect(api.commitReviewV1).not.toHaveBeenCalled();
  });

  it('freezes rail navigation during a commit and advances only from the authoritative page', async () => {
    const first = inboxSegment('aaaaaaaa-1', true);
    const second = inboxSegment('bbbbbbbb-2');
    let resolveDecision!: (value: {
      segmentId: string;
      committedRevision: number;
      authoritativeTranscript: string;
      decisionId: string;
    }) => void;
    api.getReviewPageV1
      .mockResolvedValueOnce(reviewPage([first, second]))
      .mockResolvedValue(reviewPage([second]));
    api.commitReviewV1.mockReturnValue(
      new Promise((resolve) => {
        resolveDecision = resolve;
      }),
    );
    render(ReviewInbox);
    await screen.findByText('Queue (2)');
    await waitFor(() => expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalled());
    await hearCurrentAudio();

    await fireEvent.click(screen.getByRole('button', { name: /^A Accept$/ }));
    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalled());
    expect(screen.getByRole('button', { name: /^A Accept$/ })).toHaveAccessibleDescription(
      'A review change is still being saved. Wait for it to finish.',
    );
    await fireEvent.keyDown(window, { key: 'ArrowRight', code: 'ArrowRight' });
    expect(screen.getByText('aaaaaaaa…').closest('[role="option"]')).toHaveAttribute(
      'aria-selected',
      'true',
    );
    expect(screen.getByText('bbbbbbbb…').closest('[role="option"]')).toHaveAttribute(
      'aria-selected',
      'false',
    );

    resolveDecision({
      segmentId: first.id,
      committedRevision: 8,
      authoritativeTranscript: first.rawTranscript,
      decisionId: 'effect:9',
    });
    await waitFor(() => expect(screen.getByText('Accepted')).toBeInTheDocument());
    expect(screen.queryByText('aaaaaaaa…')).not.toBeInTheDocument();
    expect(screen.getByText('bbbbbbbb…').closest('[role="option"]')).toHaveAttribute(
      'aria-selected',
      'true',
    );
  });

  it('uses one composite listbox tab stop with an active descendant and deterministic focus', async () => {
    api.getReviewPageV1.mockResolvedValue(
      reviewPage([
        inboxSegment('aaaaaaaa-1'),
        inboxSegment('bbbbbbbb-2'),
        inboxSegment('cccccccc-3'),
      ]),
    );
    render(ReviewInbox);

    const listbox = await screen.findByRole('listbox', { name: 'Queue (3)' });
    const options = within(listbox).getAllByRole('option');
    expect(listbox).toHaveAttribute('tabindex', '0');
    expect(options).toHaveLength(3);
    expect(options.every((option) => option.getAttribute('tabindex') === '-1')).toBe(true);
    expect(options[0]).toHaveAttribute('aria-selected', 'true');
    expect(listbox).toHaveAttribute('aria-activedescendant', options[0].id);

    listbox.focus();
    await fireEvent.keyDown(listbox, { key: 'ArrowDown', code: 'ArrowDown' });
    await waitFor(() => expect(options[1]).toHaveAttribute('aria-selected', 'true'));
    expect(document.activeElement).toBe(listbox);
    expect(listbox).toHaveAttribute('aria-activedescendant', options[1].id);
    expect(screen.getByTestId('inbox-active-announcement')).toHaveTextContent(
      'Active segment 2 of 3',
    );

    await fireEvent.keyDown(listbox, { key: 'End', code: 'End' });
    expect(options[2]).toHaveAttribute('aria-selected', 'true');
    await fireEvent.keyDown(listbox, { key: 'Home', code: 'Home' });
    expect(options[0]).toHaveAttribute('aria-selected', 'true');

    await fireEvent.keyDown(options[2], { key: 'Enter', code: 'Enter' });
    await waitFor(() => expect(options[2]).toHaveAttribute('aria-selected', 'true'));
    await fireEvent.keyDown(options[1], { key: ' ', code: 'Space' });
    await waitFor(() => expect(options[1]).toHaveAttribute('aria-selected', 'true'));
    await fireEvent.keyDown(options[1], { key: 'Enter', code: 'Enter' });
    expect(document.activeElement).toBe(listbox);
  });

  it('keeps the unified navigation and replay shortcuts while announcing the active item', async () => {
    api.getReviewPageV1.mockResolvedValue(
      reviewPage([
        inboxSegment('aaaaaaaa-1'),
        inboxSegment('bbbbbbbb-2'),
        inboxSegment('cccccccc-3'),
      ]),
    );
    render(ReviewInbox);

    const listbox = await screen.findByRole('listbox', { name: 'Queue (3)' });
    const options = within(listbox).getAllByRole('option');
    await fireEvent.keyDown(window, { key: 'n', code: 'KeyN' });
    await waitFor(() => expect(options[1]).toHaveAttribute('aria-selected', 'true'));
    expect(document.activeElement).toBe(listbox);

    await fireEvent.keyDown(window, { key: 'p', code: 'KeyP' });
    expect(options[0]).toHaveAttribute('aria-selected', 'true');
    await fireEvent.keyDown(window, { key: 'ArrowDown', code: 'ArrowDown' });
    expect(options[1]).toHaveAttribute('aria-selected', 'true');
    await fireEvent.keyDown(window, { key: 'ArrowUp', code: 'ArrowUp' });
    expect(options[0]).toHaveAttribute('aria-selected', 'true');

    const replay = new KeyboardEvent('keydown', {
      key: 'r',
      code: 'KeyR',
      bubbles: true,
      cancelable: true,
    });
    listbox.dispatchEvent(replay);
    expect(replay.defaultPrevented).toBe(true);
    expect(options[0]).toHaveAttribute('aria-selected', 'true');
  });

  it('does not hijack native Space or Enter on a focused button', async () => {
    api.getReviewPageV1.mockResolvedValue(reviewPage([inboxSegment('aaaaaaaa-1')]));
    render(ReviewInbox);
    await screen.findByText('Queue (1)');

    const edit = screen.getByRole('button', { name: /^E Edit$/ });
    edit.focus();
    const space = new KeyboardEvent('keydown', {
      key: ' ',
      code: 'Space',
      bubbles: true,
      cancelable: true,
    });
    edit.dispatchEvent(space);
    expect(space.defaultPrevented).toBe(false);
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();

    const enter = new KeyboardEvent('keydown', {
      key: 'Enter',
      code: 'Enter',
      bubbles: true,
      cancelable: true,
    });
    edit.dispatchEvent(enter);
    expect(enter.defaultPrevented).toBe(false);
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(api.commitReviewV1).not.toHaveBeenCalled();
  });

  it('describes why unavailable actions are disabled', async () => {
    api.getReviewPageV1.mockResolvedValue(reviewPage([inboxSegment('aaaaaaaa-1')]));
    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await waitFor(() => expect(sharedDurableReviewUndo.state.status).toBe('none'));

    const undo = screen.getByRole('button', { name: /Undo/ });
    expect(undo).toBeDisabled();
    expect(undo).toHaveAttribute('aria-describedby', 'inbox-undo-disabled-reason');
    expect(undo).toHaveAccessibleDescription(
      'There is no committed desktop review action to undo.',
    );
  });

  it('marks failed media only with an explicit reason, reuses a definitively rejected operation identity, and advances on typed success', async () => {
    const failed = inboxSegment('unusable-1', true);
    const next = inboxSegment('nextclip-2', true);
    let projectionRows = [failed, next];
    api.getReviewPageV1.mockImplementation(async () =>
      reviewPage(projectionRows, { baseRevision: 7 }),
    );
    api.registerReviewMediaAsset.mockImplementation(async (path: string) => {
      if (path.includes('unusable-1')) throw new Error('file missing');
      return { id: NEXT_MEDIA_GRANT_ID };
    });
    api.markSegmentUnusableV1
      .mockRejectedValueOnce({
        schema: 1,
        code: 'WRITE_REJECTED',
        message: 'the backend proved no write occurred',
        retryable: true,
      })
      .mockImplementationOnce(async (request) => {
        projectionRows = [next];
        undoAvailability = {
          status: 'available',
          target: flagUndoTarget(
            request.segmentId,
            request.operationId,
            request.baseRevision,
            { kind: 'technicalUnusable', reason: request.reason },
            303,
          ),
        };
        return {
          segmentId: request.segmentId,
          committedRevision: request.baseRevision + 1,
          reason: request.reason,
          effectId: 'flag-effect:303',
        };
      });

    render(ReviewInbox);
    expect(await screen.findByTestId('inbox-technical-unusable')).toBeInTheDocument();
    expect(screen.getByText('unusable-1.wav')).toBeInTheDocument();
    const reason = screen.getByLabelText('Technical reason');
    const mark = screen.getByRole('button', { name: 'Mark technically unusable' });
    expect(mark).toBeDisabled();
    reason.focus();
    await fireEvent.keyDown(reason, { key: 'ArrowDown', code: 'ArrowDown' });
    expect(screen.getByText('unusable-1.wav')).toBeInTheDocument();
    expect(screen.queryByText('nextclip-2.wav')).not.toBeInTheDocument();
    await fireEvent.change(reason, { target: { value: 'corruptContainer' } });
    expect(mark).toBeEnabled();

    mark.focus();
    await fireEvent.click(mark);
    await waitFor(() => expect(api.markSegmentUnusableV1).toHaveBeenCalledTimes(1));
    expect(screen.getByText('unusable-1.wav')).toBeInTheDocument();
    expect(reason).toHaveValue('corruptContainer');
    expect(document.activeElement).toBe(mark);
    expect(api.recordPlaybackReceipt).not.toHaveBeenCalled();
    expect(api.commitReviewV1).not.toHaveBeenCalled();

    await fireEvent.click(mark);
    await waitFor(() => expect(api.markSegmentUnusableV1).toHaveBeenCalledTimes(2));
    const first = api.markSegmentUnusableV1.mock.calls[0][0];
    const replay = api.markSegmentUnusableV1.mock.calls[1][0];
    expect(first).toEqual({
      operationId: expect.stringMatching(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
      ),
      segmentId: failed.id,
      baseRevision: 7,
      reason: 'corruptContainer',
    });
    expect(replay).toEqual(first);
    await waitFor(() => expect(screen.getByText('nextclip-2.wav')).toBeInTheDocument());
    expect(screen.queryByText('unusable-1.wav')).not.toBeInTheDocument();
  });

  it.each(['STALE_REVISION', 'HUMAN_TRUTH_ALREADY_COMMITTED', 'SEGMENT_NOT_FOUND'])(
    'reloads %s technical-unusable authority and retries with the new revision and a new operation identity',
    async (refusalCode) => {
      const failed = inboxSegment('unusable-stale-1', true);
      const next = inboxSegment('unusable-stale-next-2', true);
      let projectionRows = [failed, next];
      let baseRevision = 7;
      api.getReviewPageV1.mockImplementation(async () =>
        reviewPage(projectionRows, { baseRevision }),
      );
      api.registerReviewMediaAsset.mockImplementation(async (path: string) => {
        if (path.includes(failed.id)) throw new Error('file missing');
        return { id: NEXT_MEDIA_GRANT_ID };
      });
      api.markSegmentUnusableV1
        .mockImplementationOnce(async () => {
          baseRevision = 8;
          throw {
            schema: 1,
            code: refusalCode,
            message: 'This clip changed; reload it before marking it unusable.',
            retryable: false,
            suggestedAction: 'reloadClip',
            details: { expectedRevision: 7, currentRevision: 8 },
          };
        })
        .mockImplementationOnce(async (request) => {
          projectionRows = [next];
          undoAvailability = {
            status: 'available',
            target: flagUndoTarget(
              request.segmentId,
              request.operationId,
              request.baseRevision,
              { kind: 'technicalUnusable', reason: request.reason },
              304,
            ),
          };
          return {
            segmentId: request.segmentId,
            committedRevision: request.baseRevision + 1,
            reason: request.reason,
            effectId: 'flag-effect:304',
          };
        });

      render(ReviewInbox);
      expect(await screen.findByTestId('inbox-technical-unusable')).toBeInTheDocument();
      await fireEvent.change(screen.getByLabelText('Technical reason'), {
        target: { value: 'missingFile' },
      });
      await fireEvent.click(screen.getByRole('button', { name: 'Mark technically unusable' }));

      await waitFor(() => expect(api.getReviewPageV1).toHaveBeenCalledTimes(2));
      expect(screen.getByText(`${failed.id}.wav`)).toBeInTheDocument();
      expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(false);
      expect(await screen.findByTestId('inbox-technical-unusable')).toBeInTheDocument();

      await fireEvent.change(screen.getByLabelText('Technical reason'), {
        target: { value: 'missingFile' },
      });
      await fireEvent.click(screen.getByRole('button', { name: 'Mark technically unusable' }));

      await waitFor(() => expect(api.markSegmentUnusableV1).toHaveBeenCalledTimes(2));
      const first = api.markSegmentUnusableV1.mock.calls[0][0];
      const retry = api.markSegmentUnusableV1.mock.calls[1][0];
      expect(first).toMatchObject({ segmentId: failed.id, baseRevision: 7, reason: 'missingFile' });
      expect(retry).toMatchObject({ segmentId: failed.id, baseRevision: 8, reason: 'missingFile' });
      expect(retry.operationId).not.toBe(first.operationId);
      await waitFor(() => expect(screen.getByText(`${next.id}.wav`)).toBeInTheDocument());
    },
  );

  it('localizes queue, audio, duration, and active-item accessibility text in Sorani', async () => {
    locale.set('ckb');
    api.getReviewPageV1.mockResolvedValue(reviewPage([inboxSegment('aaaaaaaa-1')]));
    render(ReviewInbox);

    const listbox = await screen.findByRole('listbox', { name: 'ڕیز (1)' });
    expect(within(listbox).getByRole('option')).toHaveAccessibleName('پارچەی 1 لە 1: aaaaaaaa-1');
    expect(screen.getByLabelText('لێدانی دەنگ')).toBeInTheDocument();
    expect(screen.getByText('دەنگ بەردەست نییە')).toBeInTheDocument();
    expect(screen.getByText('1 چرکە')).toBeInTheDocument();
    expect(screen.queryByLabelText('Audio playback')).not.toBeInTheDocument();
  });

  it('exposes the selected autonomy level as a pressed button', async () => {
    api.getReviewPageV1.mockResolvedValue(reviewPage([]));
    render(ReviewInbox);
    await screen.findByText('Inbox zero!');

    const group = screen.getByRole('group', { name: 'Autonomy level' });
    const propose = within(group).getByRole('button', { name: /Propose/ });
    const observe = within(group).getByRole('button', { name: /Observe/ });
    expect(propose).toHaveAttribute('aria-pressed', 'true');
    expect(observe).toHaveAttribute('aria-pressed', 'false');

    await fireEvent.click(observe);
    expect(observe).toHaveAttribute('aria-pressed', 'true');
    expect(propose).toHaveAttribute('aria-pressed', 'false');
  });

  it('persists autonomy changes and rolls the dial back when persistence fails', async () => {
    const row = inboxSegment('autonomy-row');
    api.getReviewPageV1.mockResolvedValue(reviewPage([row]));
    api.getSettings.mockResolvedValue({ ...defaultSettings, juryAutonomyLevel: 'propose' });
    api.updateSettings
      .mockResolvedValueOnce(undefined)
      .mockRejectedValueOnce(new Error('settings write failed'));

    render(ReviewInbox);
    await screen.findByTestId('jury-local-only');
    const group = screen.getByRole('group', { name: 'Autonomy level' });
    const observe = within(group).getByRole('button', { name: /Observe/ });
    const actAuto = within(group).getByRole('button', { name: /Act Auto/ });

    await fireEvent.click(observe);
    await waitFor(() => expect(api.updateSettings).toHaveBeenCalledTimes(1));
    expect(api.updateSettings).toHaveBeenLastCalledWith(
      expect.objectContaining({ juryAutonomyLevel: 'observe' }),
    );
    expect(await screen.findByText('Autonomy set to Observe')).toBeInTheDocument();
    expect(observe).toHaveAttribute('aria-pressed', 'true');

    await fireEvent.click(actAuto);
    await waitFor(() => expect(api.updateSettings).toHaveBeenCalledTimes(2));
    expect(await screen.findByText(/Failed to change autonomy:/)).toBeInTheDocument();
    expect(observe).toHaveAttribute('aria-pressed', 'true');
    expect(actAuto).toHaveAttribute('aria-pressed', 'false');
  });

  it('holds one truth lease for Jury, blocks every competing action, then reloads and reopens', async () => {
    const onClose = vi.fn();
    const first = inboxSegment('jury-held-first', true);
    const second = inboxSegment('jury-held-second', true);
    const jury = deferred<{
      t0AutoAccepted: number;
      t1Committed: number;
      t2Committed: number;
      humanInbox: number;
    }>();
    api.getReviewPageV1.mockImplementation(async () => reviewPage([first, second]));
    api.getSegmentIdsForView.mockResolvedValue([first.id, second.id]);
    api.runJuryPipeline.mockReturnValue(jury.promise);
    undoAvailability = {
      status: 'available',
      target: undoTarget(
        'prior-reviewed-segment',
        'accept',
        '8e23f406-953c-4ba4-9c70-408a16bc0d6b',
        404,
      ),
    };

    render(ReviewInbox, { onClose });
    await screen.findByText('Queue (2)');
    await waitFor(() => expect(sharedDurableReviewUndo.state.status).toBe('ready'));
    await hearCurrentAudio();

    await fireEvent.click(screen.getByRole('button', { name: 'Run Jury' }));
    await waitFor(() => expect(api.runJuryPipeline).toHaveBeenCalledWith([first.id, second.id]));
    expect(sharedDurableReviewUndo.state.truthWriteInFlight).toBe(true);

    const accept = screen.getByRole('button', { name: /^A Accept$/ });
    const edit = screen.getByRole('button', { name: /^E Edit$/ });
    const flag = screen.getByRole('button', { name: /^F Flag$/ });
    const undo = screen.getByRole('button', { name: /Undo/ });
    const close = screen.getByRole('button', { name: 'Close inbox' });
    for (const action of [accept, edit, flag, undo, close]) expect(action).toBeDisabled();

    await fireEvent.click(accept);
    await fireEvent.click(flag);
    await fireEvent.click(undo);
    await fireEvent.click(close);
    await fireEvent.keyDown(window, { key: 'a', code: 'KeyA' });
    await fireEvent.keyDown(window, { key: 'f', code: 'KeyF' });
    await fireEvent.keyDown(window, { key: 'Backspace', code: 'Backspace' });
    await fireEvent.keyDown(window, { key: 'Escape', code: 'Escape' });

    const firstOption = screen.getByRole('option', { name: /jury-held-first/ });
    const secondOption = screen.getByRole('option', { name: /jury-held-second/ });
    await fireEvent.click(secondOption);
    await fireEvent.keyDown(window, { key: 'ArrowDown', code: 'ArrowDown' });

    expect(firstOption).toHaveAttribute('aria-selected', 'true');
    expect(secondOption).toHaveAttribute('aria-selected', 'false');
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(api.commitReviewV1).not.toHaveBeenCalled();
    expect(api.recordReviewFlag).not.toHaveBeenCalled();
    expect(api.undoDesktopReviewActionV1).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
    expect(api.getReviewPageV1).toHaveBeenCalledTimes(1);

    jury.resolve({ t0AutoAccepted: 0, t1Committed: 0, t2Committed: 0, humanInbox: 2 });
    await waitFor(() => expect(api.getReviewPageV1).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(sharedDurableReviewUndo.state.truthWriteInFlight).toBe(false));
    expect(
      await screen.findByText(
        'Jury finished. T0 accepted: 0, T1 committed: 0, T2 committed: 0, Escalated: 2',
      ),
    ).toBeInTheDocument();
    const reopened = [
      screen.getByRole('button', { name: /^A Accept$/ }),
      screen.getByRole('button', { name: /^E Edit$/ }),
      screen.getByRole('button', { name: /^F Flag$/ }),
      screen.getByRole('button', { name: /Undo/ }),
      screen.getByRole('button', { name: 'Close inbox' }),
    ];
    await waitFor(() => {
      for (const action of reopened) expect(action).toBeEnabled();
    });

    await fireEvent.click(reopened[4]);
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
  });

  it('times out target discovery without invoking Jury and ignores its late resolution after retry', async () => {
    const row = inboxSegment('jury-discovery-timeout');
    const firstDiscovery = deferred<string[]>();
    api.getReviewPageV1.mockResolvedValue(reviewPage([row]));
    api.getSegmentIdsForView.mockReturnValueOnce(firstDiscovery.promise).mockResolvedValueOnce([]);

    render(ReviewInbox);
    await screen.findByText('Queue (1)');

    vi.useFakeTimers();
    try {
      await fireEvent.click(screen.getByRole('button', { name: 'Run Jury' }));
      await vi.advanceTimersByTimeAsync(0);
      expect(api.getSegmentIdsForView).toHaveBeenCalledOnce();
      expect(api.runJuryPipeline).not.toHaveBeenCalled();
      expect(sharedDurableReviewUndo.state.truthWriteInFlight).toBe(true);

      await vi.advanceTimersByTimeAsync(15_000);
      expect(sharedDurableReviewUndo.state.truthWriteInFlight).toBe(false);
      expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(false);
      expect(api.runJuryPipeline).not.toHaveBeenCalled();
      expect(
        screen
          .getAllByRole('status')
          .some((status) =>
            status.textContent?.includes('Jury pipeline failed: E_JURY_TARGET_DISCOVERY_TIMEOUT'),
          ),
      ).toBe(true);
      expect(screen.getByRole('button', { name: 'Run Jury' })).toBeEnabled();

      await fireEvent.click(screen.getByRole('button', { name: 'Run Jury' }));
      await vi.advanceTimersByTimeAsync(0);
      expect(api.getSegmentIdsForView).toHaveBeenCalledTimes(2);
      expect(api.runJuryPipeline).not.toHaveBeenCalled();
      expect(sharedDurableReviewUndo.state.truthWriteInFlight).toBe(false);
      expect(
        screen
          .getAllByRole('status')
          .some((status) => status.textContent === 'No unverified segments to run jury on.'),
      ).toBe(true);

      firstDiscovery.resolve([row.id]);
      await vi.advanceTimersByTimeAsync(0);
      expect(api.runJuryPipeline).not.toHaveBeenCalled();
      expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(false);
      expect(
        screen
          .getAllByRole('status')
          .some((status) => status.textContent === 'No unverified segments to run jury on.'),
      ).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it('hard-stops a timed-out Jury writer and ignores its late success without reopening truth', async () => {
    const onClose = vi.fn();
    const first = inboxSegment('jury-writer-timeout-a', true);
    const second = inboxSegment('jury-writer-timeout-b', true);
    const writer = deferred<{
      t0AutoAccepted: number;
      t1Committed: number;
      t2Committed: number;
      humanInbox: number;
    }>();
    api.getReviewPageV1.mockResolvedValue(reviewPage([first, second]));
    api.getSegmentIdsForView.mockResolvedValue([first.id, second.id]);
    api.runJuryPipeline.mockReturnValue(writer.promise);
    undoAvailability = {
      status: 'available',
      target: undoTarget(
        'prior-reviewed-segment',
        'accept',
        'ec0884fe-48fc-49d8-ac1d-a7fcfd4fa241',
        505,
      ),
    };

    render(ReviewInbox, { onClose });
    await screen.findByText('Queue (2)');
    await waitFor(() => expect(sharedDurableReviewUndo.state.status).toBe('ready'));
    await hearCurrentAudio();

    vi.useFakeTimers();
    try {
      await fireEvent.click(screen.getByRole('button', { name: 'Run Jury' }));
      await vi.advanceTimersByTimeAsync(0);
      expect(api.runJuryPipeline).toHaveBeenCalledOnce();
      expect(sharedDurableReviewUndo.state.truthWriteInFlight).toBe(true);

      await vi.advanceTimersByTimeAsync(15_000);
      expect(sharedDurableReviewUndo.state.truthWriteInFlight).toBe(false);
      expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(true);
      expect(sharedDurableReviewUndo.state.errorCode).toBe('E_JURY_PIPELINE_TIMEOUT');
      expect(api.getReviewPageV1).toHaveBeenCalledOnce();

      const runJury = screen.getByRole('button', { name: 'Run Jury' });
      const accept = screen.getByRole('button', { name: /^A Accept$/ });
      const flag = screen.getByRole('button', { name: /^F Flag$/ });
      const undo = screen.getByRole('button', { name: /Undo/ });
      for (const action of [runJury, accept, flag, undo]) expect(action).toBeDisabled();

      await fireEvent.click(accept);
      await fireEvent.click(flag);
      await fireEvent.click(undo);
      await fireEvent.click(screen.getByRole('button', { name: 'Close inbox' }));
      await fireEvent.keyDown(window, { key: 'a', code: 'KeyA' });
      await fireEvent.keyDown(window, { key: 'f', code: 'KeyF' });
      await fireEvent.keyDown(window, { key: 'Backspace', code: 'Backspace' });
      await fireEvent.keyDown(window, { key: 'Escape', code: 'Escape' });
      const firstOption = screen.getByRole('option', { name: /jury-writer-timeout-a/ });
      const secondOption = screen.getByRole('option', { name: /jury-writer-timeout-b/ });
      await fireEvent.click(secondOption);
      await fireEvent.keyDown(window, { key: 'ArrowDown', code: 'ArrowDown' });

      expect(firstOption).toHaveAttribute('aria-selected', 'true');
      expect(secondOption).toHaveAttribute('aria-selected', 'false');
      expect(api.commitReviewV1).not.toHaveBeenCalled();
      expect(api.recordReviewFlag).not.toHaveBeenCalled();
      expect(api.undoDesktopReviewActionV1).not.toHaveBeenCalled();
      expect(onClose).not.toHaveBeenCalled();

      writer.resolve({ t0AutoAccepted: 1, t1Committed: 0, t2Committed: 0, humanInbox: 1 });
      await vi.advanceTimersByTimeAsync(0);
      expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(true);
      expect(sharedDurableReviewUndo.state.errorCode).toBe('E_JURY_PIPELINE_TIMEOUT');
      expect(api.getReviewPageV1).toHaveBeenCalledOnce();
      expect(runJury).toBeDisabled();
    } finally {
      vi.useRealTimers();
    }
  });

  it('reports a jury no-op without invoking the pipeline', async () => {
    api.getReviewPageV1.mockResolvedValue(reviewPage([inboxSegment('jury-noop')]));
    api.getSegmentIdsForView.mockResolvedValue([]);

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await fireEvent.click(screen.getByRole('button', { name: 'Run Jury' }));

    expect(await screen.findByText('No unverified segments to run jury on.')).toBeInTheDocument();
    expect(api.runJuryPipeline).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Run Jury' })).toBeEnabled();
  });

  it('hard-stops when the Jury returns no report after its mutation IPC', async () => {
    api.getReviewPageV1.mockResolvedValue(reviewPage([inboxSegment('jury-null')]));
    api.getSegmentIdsForView.mockResolvedValue(['jury-null']);
    api.runJuryPipeline.mockResolvedValue(null);

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await fireEvent.click(screen.getByRole('button', { name: 'Run Jury' }));

    await waitFor(() => expect(sharedDurableReviewUndo.state.truthWriteAmbiguous).toBe(true));
    const runJury = screen.getByRole('button', { name: 'Run Jury' });
    expect(runJury).toBeDisabled();
    expect(runJury).toHaveAccessibleDescription(
      'The save result is uncertain after two exact attempts. All further decisions are blocked. Restart Cortex to reopen from database truth; your draft is retained.',
    );
    await fireEvent.click(runJury);
    expect(api.runJuryPipeline).toHaveBeenCalledOnce();
    expect(api.getReviewPageV1).toHaveBeenCalledOnce();
  });

  it('defaults legacy autonomy safely and zero-fills an otherwise valid empty jury report', async () => {
    const row = inboxSegment('jury-empty-report');
    api.getReviewPageV1.mockResolvedValue(reviewPage([row]));
    api.getSettings.mockResolvedValue({
      ...defaultSettings,
      juryAutonomyLevel: undefined,
    });
    api.getSegmentIdsForView.mockResolvedValue([row.id]);
    api.runJuryPipeline.mockResolvedValue({});

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    const group = screen.getByRole('group', { name: 'Autonomy level' });
    expect(within(group).getByRole('button', { name: /Propose/ })).toHaveAttribute(
      'aria-pressed',
      'true',
    );

    await fireEvent.click(screen.getByRole('button', { name: 'Run Jury' }));
    await waitFor(() => expect(api.getReviewPageV1).toHaveBeenCalledTimes(2));
    await waitFor(() =>
      expect(
        screen.getByText(
          'Jury finished. T0 accepted: 0, T1 committed: 0, T2 committed: 0, Escalated: 0',
        ),
      ).toBeInTheDocument(),
    );
  });

  it('keeps the inbox open and restores editor focus when the close draft barrier fails', async () => {
    const onClose = vi.fn();
    api.getReviewPageV1.mockResolvedValue(reviewPage([inboxSegment('close-failure')]));
    api.saveReviewDraftV1.mockRejectedValue(new Error('disk full'));

    render(ReviewInbox, { onClose });
    await screen.findByText('Queue (1)');
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    const editor = screen.getByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'must remain recoverable' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Close inbox' }));

    expect(
      await screen.findByText('Close paused — the review draft is not safely stored'),
    ).toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
    await waitFor(() => expect(document.activeElement).toBe(editor));
    expect(editor).toHaveValue('must remain recoverable');
  });
});

describe('ReviewInbox locale contract', () => {
  it('keeps every inbox translation key in exact English/Sorani parity', () => {
    const inboxKeys = (messages: Record<string, string>) =>
      Object.keys(messages)
        .filter((key) => key.startsWith('inbox.'))
        .sort();
    expect(inboxKeys(ckb)).toEqual(inboxKeys(en));
  });
});

describe('safeInboxEvidence', () => {
  it('pretty-prints valid JSON evidence and maps absent evidence to an empty array', () => {
    expect(safeInboxEvidence('{"model":"champion","score":1}')).toBe(
      '{\n  "model": "champion",\n  "score": 1\n}',
    );
    expect(safeInboxEvidence(null)).toBe('[]');
  });

  it('preserves malformed evidence verbatim without throwing', () => {
    expect(safeInboxEvidence('{not-json')).toBe('{not-json');
    expect(safeInboxEvidence(undefined)).toBe('[]');
  });
});
